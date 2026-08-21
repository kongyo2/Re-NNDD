//! yt-dlp のアップデート追従。
//!
//! niconico は動画配信の仕様をちょくちょく変える。追従しているのは yt-dlp 側
//! なので、同梱した yt-dlp が古くなるとアプリごと DL 不能になる。アプリの
//! リリースを待たずに yt-dlp だけ更新できる導線を用意する。
//!
//! 設計は Parabolic (Nickvision) の `YtdlpExecutableService` を踏襲:
//!
//! - stable は `yt-dlp/yt-dlp`、nightly は `yt-dlp/yt-dlp-nightly-builds` を見る
//! - OS / アーキテクチャごとの単体バイナリ資産名を選ぶ
//! - DL した物は `<app_data_dir>/bin/` に置き、同梱版より優先して使う
//!   ([`crate::downloader::tools::resolve`])
//! - 入れた版は `settings` テーブルに記録して次回起動でも判るようにする
//!
//! Parabolic から足したところ:
//!
//! - GitHub API が塞がれている / レート制限に当たっている環境向けに、
//!   `releases/latest/download/...` の 302 から tag を読む経路をフォールバック
//!   として持つ (API 版は認証なしだと 60 req/h でよく枯れる)
//! - リリース同梱の `SHA2-256SUMS` と突き合わせて改竄・破損を弾く
//! - 入れ替え前に `--version` を実行して、動く物であることを確認してから
//!   本番パスへ rename する (壊れた DL で既存の yt-dlp を潰さない)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use tauri::State;
use tokio::io::AsyncWriteExt;

use crate::downloader::tools;
use crate::error::{ApiError, AppError, Result};
use crate::library::db::LibraryHandle;
use crate::library::settings;

// ===================== 設定キー =====================

/// 実際に入れた yt-dlp の版 (バイナリの `--version` 出力)。
pub const KEY_INSTALLED_VERSION: &str = "ytdlp.installed_version";
/// 追従するチャンネル (`stable` / `nightly`)。
pub const KEY_CHANNEL: &str = "ytdlp.update_channel";
/// 起動時に自動でチェックするか。
pub const KEY_AUTO_CHECK: &str = "ytdlp.auto_check";
/// 新版を見つけたら確認なしで入れるか。
pub const KEY_AUTO_INSTALL: &str = "ytdlp.auto_install";
/// 最後にチェックした時刻 (Unix 秒)。
pub const KEY_LAST_CHECKED_AT: &str = "ytdlp.last_checked_at";
/// 最後のチェックで見えた最新版。
pub const KEY_LATEST_KNOWN: &str = "ytdlp.latest_known_version";

/// 自動チェックの間隔。yt-dlp の stable は数週間に 1 回、nightly は毎日なので
/// 起動のたびに叩く必要はない。
const AUTO_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

// ===================== バージョン =====================

/// yt-dlp の版。
///
/// stable は `2026.08.19` の 3 要素、nightly は `2026.08.20.234504`
/// (= yyyy.mm.dd.HHMMSS) の 4 要素。4 要素目が 0 でなければ nightly と見なす
/// (Parabolic が `AppVersion.Revision > 0` でプレビュー判定しているのと同じ)。
///
/// 比較は数値タプル、表示は元の文字列。`2026.08.19` を再構築しようとすると
/// ゼロ詰めの桁数を推測する羽目になるので、素直に生文字列を持っておく。
#[derive(Debug, Clone)]
pub struct YtdlpVersion {
    parts: [u64; 4],
    raw: String,
}

impl YtdlpVersion {
    /// `--version` 出力や GitHub の tag 名から版を読む。
    ///
    /// 先頭の空でない行の最初のトークンだけを見る (`2026.08.19 (dev)` のような
    /// 出力にも耐えるため)。`v` 接頭辞は落とす。
    pub fn parse(s: &str) -> Option<Self> {
        let token = s
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())?
            .split_whitespace()
            .next()?;
        let body = token
            .strip_prefix('v')
            .or_else(|| token.strip_prefix('V'))
            .unwrap_or(token);
        let mut parts = [0u64; 4];
        let mut count = 0usize;
        for seg in body.split('.') {
            if count >= 4 {
                return None;
            }
            // 空セグメント (`2026..19`) や非数字 (`2026.08.19-rc1`) は弾く。
            // 桁数を絞っておけば u64 が溢れることもない。
            if seg.is_empty() || seg.len() > 12 || !seg.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            parts[count] = seg.parse::<u64>().ok()?;
            count += 1;
        }
        if count < 3 {
            return None;
        }
        Some(Self {
            parts,
            raw: body.to_string(),
        })
    }

    /// nightly ビルドか (= 4 要素目を持つか)。
    pub fn is_nightly(&self) -> bool {
        self.parts[3] > 0
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl PartialEq for YtdlpVersion {
    fn eq(&self, other: &Self) -> bool {
        self.parts == other.parts
    }
}
impl Eq for YtdlpVersion {}
impl PartialOrd for YtdlpVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for YtdlpVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parts.cmp(&other.parts)
    }
}
impl std::fmt::Display for YtdlpVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}
impl Serialize for YtdlpVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

// ===================== チャンネル =====================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Nightly,
}

impl UpdateChannel {
    /// 設定値 (自由文字列) から。未知の値は stable 扱い。
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("nightly") {
            Self::Nightly
        } else {
            Self::Stable
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }

    /// リリースを配っている GitHub リポジトリ (`owner/repo`)。
    pub fn repo(self) -> &'static str {
        match self {
            Self::Stable => "yt-dlp/yt-dlp",
            Self::Nightly => "yt-dlp/yt-dlp-nightly-builds",
        }
    }
}

// ===================== 資産名 =====================

/// この OS / アーキテクチャで落とすべきリリース資産の名前。
///
/// yt-dlp のリリースには単体実行ファイル (PyInstaller) と、Python 前提の
/// zipimport 版 (`yt-dlp`) がある。素の `yt-dlp` は Python が要るので、
/// 単体で動く物がある環境ではそちらを選ぶ。
pub const fn asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            "yt-dlp_arm64.exe"
        } else if cfg!(target_arch = "x86") {
            "yt-dlp_x86.exe"
        } else {
            "yt-dlp.exe"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "yt-dlp_linux_aarch64"
        } else if cfg!(target_arch = "x86_64") {
            "yt-dlp_linux"
        } else {
            // armv7 などは単体バイナリが zip でしか配られていない。
            // Python 前提の zipimport 版に落とす。
            "yt-dlp"
        }
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else {
        "yt-dlp"
    }
}

/// リリースに同梱されている SHA-256 一覧のファイル名。
const SUMS_ASSET: &str = "SHA2-256SUMS";

// ===================== リリース情報 =====================

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: YtdlpVersion,
    /// この OS 向け単体バイナリの DL URL。
    pub asset_url: String,
    /// `SHA2-256SUMS` の URL。
    pub sums_url: String,
    /// リリースページ (UI から開く用)。
    pub release_url: String,
    /// どの経路で解決したか (`api` / `redirect`)。診断用。
    pub via: &'static str,
}

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_WEB_BASE: &str = "https://github.com";

/// GitHub リリースを見に行くクライアント。
pub struct UpdateClient {
    /// リダイレクトを追う方 (API 呼び出し / 資産 DL)。
    http: reqwest::Client,
    /// リダイレクトを追わない方 (302 の Location から tag を読む)。
    probe: reqwest::Client,
    /// `https://api.github.com`。テストで差し替える。
    api_base: String,
    /// `https://github.com`。テストで差し替える。
    web_base: String,
}

fn user_agent() -> String {
    format!(
        "Re-NNDD/{} (+https://github.com/abeshinzo78/Re-NNDD)",
        env!("CARGO_PKG_VERSION")
    )
}

impl UpdateClient {
    pub fn new() -> std::result::Result<Self, ApiError> {
        Self::with_bases(GITHUB_API_BASE, GITHUB_WEB_BASE)
    }

    /// 接続先を差し替えられる構築子 (テスト用。本番は [`UpdateClient::new`])。
    fn with_bases(api_base: &str, web_base: &str) -> std::result::Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(15))
            // 資産 DL は回線次第で数分かかる。全体タイムアウトではなく
            // 「無音が続いたら諦める」read timeout で止める。
            .read_timeout(Duration::from_secs(60))
            .build()?;
        let probe = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            probe,
            api_base: api_base.trim_end_matches('/').to_string(),
            web_base: web_base.trim_end_matches('/').to_string(),
        })
    }

    /// チャンネルの最新リリースを取る。
    ///
    /// GitHub API を先に試し、駄目なら 302 経由にフォールバックする。
    pub async fn latest_release(
        &self,
        channel: UpdateChannel,
    ) -> std::result::Result<ReleaseInfo, ApiError> {
        match self.latest_via_api(channel).await {
            Ok(info) => Ok(info),
            Err(api_err) => {
                tracing::warn!(
                    error = %api_err,
                    "GitHub API で yt-dlp の最新版を取れなかった。リダイレクト経由で再試行する"
                );
                self.latest_via_redirect(channel).await.map_err(|redirect_err| {
                    ApiError::Downloader(format!(
                        "yt-dlp の最新版を取得できませんでした (API: {api_err} / リダイレクト: {redirect_err})"
                    ))
                })
            }
        }
    }

    async fn latest_via_api(
        &self,
        channel: UpdateChannel,
    ) -> std::result::Result<ReleaseInfo, ApiError> {
        #[derive(Deserialize)]
        struct ApiAsset {
            name: String,
            browser_download_url: String,
        }
        #[derive(Deserialize)]
        struct ApiRelease {
            tag_name: String,
            html_url: Option<String>,
            #[serde(default)]
            assets: Vec<ApiAsset>,
        }

        let repo = channel.repo();
        let url = format!("{}/repos/{repo}/releases/latest", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let preview: String = body.chars().take(200).collect();
            return Err(ApiError::ServerError {
                status: status.as_u16(),
                message: format!("GET {url} -> {status}: {preview}"),
            });
        }
        let release: ApiRelease = resp.json().await?;
        let version = YtdlpVersion::parse(&release.tag_name).ok_or_else(|| {
            ApiError::ResponseShape(format!("tag 名を版として読めない: {}", release.tag_name))
        })?;
        let wanted = asset_name();
        let asset_url = release
            .assets
            .iter()
            .find(|a| a.name == wanted)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| {
                ApiError::ResponseShape(format!(
                    "リリース {} に {wanted} が含まれていない",
                    release.tag_name
                ))
            })?;
        let sums_url = release
            .assets
            .iter()
            .find(|a| a.name == SUMS_ASSET)
            .map(|a| a.browser_download_url.clone())
            .unwrap_or_else(|| download_url(&self.web_base, repo, &release.tag_name, SUMS_ASSET));
        Ok(ReleaseInfo {
            release_url: release.html_url.unwrap_or_else(|| {
                format!("{}/{repo}/releases/tag/{}", self.web_base, release.tag_name)
            }),
            version,
            asset_url,
            sums_url,
            via: "api",
        })
    }

    /// `releases/latest/download/<file>` は必ず
    /// `releases/download/<tag>/<file>` へ 302 する。この Location から tag を
    /// 読めば API を叩かずに最新版が判る (レート制限も認証も無い)。
    async fn latest_via_redirect(
        &self,
        channel: UpdateChannel,
    ) -> std::result::Result<ReleaseInfo, ApiError> {
        let repo = channel.repo();
        let probe_url = format!(
            "{}/{repo}/releases/latest/download/{SUMS_ASSET}",
            self.web_base
        );
        let resp = self.probe.get(&probe_url).send().await?;
        let status = resp.status();
        if !status.is_redirection() {
            return Err(ApiError::ServerError {
                status: status.as_u16(),
                message: format!("GET {probe_url} がリダイレクトしなかった ({status})"),
            });
        }
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::ResponseShape(format!("{probe_url} に Location が無い")))?;
        let tag = tag_from_download_url(location).ok_or_else(|| {
            ApiError::ResponseShape(format!("Location から tag を読めない: {location}"))
        })?;
        let version = YtdlpVersion::parse(tag)
            .ok_or_else(|| ApiError::ResponseShape(format!("tag 名を版として読めない: {tag}")))?;
        Ok(ReleaseInfo {
            asset_url: download_url(&self.web_base, repo, tag, asset_name()),
            sums_url: download_url(&self.web_base, repo, tag, SUMS_ASSET),
            release_url: format!("{}/{repo}/releases/tag/{tag}", self.web_base),
            version,
            via: "redirect",
        })
    }

    /// リリース同梱の `SHA2-256SUMS` から、この OS 向け資産の期待ハッシュを引く。
    ///
    /// 取れなかった場合は `None`。整合性チェックは「取れたら必ず照合する」
    /// 運用にして、取れないだけで更新を止めることはしない (古いリリースには
    /// SUMS が無い)。
    pub async fn expected_sha256(&self, release: &ReleaseInfo) -> Option<String> {
        let resp = self
            .http
            .get(&release.sums_url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body = resp.text().await.ok()?;
        find_sha256(&body, asset_name())
    }

    /// 資産を `dest` に落とす。SHA-256 を計算しながら書く。
    ///
    /// `on_progress(downloaded, total)` は 128 KiB ごと + 完了時に呼ばれる。
    pub async fn download_asset<F>(
        &self,
        url: &str,
        dest: &Path,
        mut on_progress: F,
    ) -> std::result::Result<DownloadedAsset, ApiError>
    where
        F: FnMut(u64, Option<u64>) + Send,
    {
        let mut resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ApiError::ServerError {
                status: status.as_u16(),
                message: format!("GET {url} -> {status}"),
            });
        }
        let total = resp.content_length();
        let mut file = tokio::fs::File::create(dest).await?;
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;
        let mut last_reported: u64 = 0;
        on_progress(0, total);
        while let Some(chunk) = resp.chunk().await? {
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            if downloaded - last_reported >= 128 * 1024 {
                last_reported = downloaded;
                on_progress(downloaded, total);
            }
        }
        // ここで flush/sync しておかないと、直後の `--version` 実行で
        // 中途半端な内容を掴む可能性がある。
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        on_progress(downloaded, total);
        Ok(DownloadedAsset {
            bytes: downloaded,
            sha256: hex_lower(&hasher.finalize()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct DownloadedAsset {
    pub bytes: u64,
    pub sha256: String,
}

fn download_url(web_base: &str, repo: &str, tag: &str, asset: &str) -> String {
    format!("{web_base}/{repo}/releases/download/{tag}/{asset}")
}

/// `https://github.com/o/r/releases/download/<tag>/<file>` から `<tag>` を抜く。
fn tag_from_download_url(url: &str) -> Option<&str> {
    let rest = url.split("/releases/download/").nth(1)?;
    let tag = rest.split('/').next()?;
    if tag.is_empty() {
        None
    } else {
        Some(tag)
    }
}

/// `<sha256>  <filename>` 形式の一覧から、指定ファイルのハッシュを引く。
fn find_sha256(body: &str, filename: &str) -> Option<String> {
    for line in body.lines() {
        let mut it = line.split_whitespace();
        let (Some(hash), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        // `sha256sum` はバイナリモードだと名前の頭に `*` を付ける。
        let name = name.strip_prefix('*').unwrap_or(name);
        if name == filename && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            // String への write! は失敗しない。
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

// ===================== 進捗の共有状態 =====================

/// インストールの進行状況。フロントは `ytdlp_update_status` を
/// 低頻度ポーリングして読む (このアプリは DL キューも同じ流儀)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    /// `idle` / `checking` / `downloading` / `verifying` / `installing` / `done` / `error`
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    /// 対象の版 (判っていれば)。
    pub version: Option<String>,
    /// 人間向けの補足 (エラー文言など)。
    pub message: Option<String>,
}

impl Default for UpdateProgress {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            downloaded_bytes: 0,
            total_bytes: None,
            version: None,
            message: None,
        }
    }
}

/// インストールの進捗。
///
/// 二重インストール防止と「DL 中に差し替えない」の排他は
/// [`crate::commands::DownloadTasks::try_begin_binary_swap`] が一手に引き受ける
/// (走っている DL と同じ Mutex で見ないと check-then-act の隙間が空くため)。
/// ここは表示用の状態だけを持つ。
#[derive(Default)]
pub struct YtdlpUpdateState {
    progress: Mutex<UpdateProgress>,
}

impl YtdlpUpdateState {
    fn snapshot(&self) -> UpdateProgress {
        self.progress.lock().clone()
    }

    fn set(&self, progress: UpdateProgress) {
        *self.progress.lock() = progress;
    }

    fn set_phase(&self, phase: &str, version: Option<&str>) {
        let mut p = self.progress.lock();
        p.phase = phase.to_string();
        if let Some(v) = version {
            p.version = Some(v.to_string());
        }
        p.message = None;
    }

    fn set_bytes(&self, downloaded: u64, total: Option<u64>) {
        let mut p = self.progress.lock();
        p.downloaded_bytes = downloaded;
        p.total_bytes = total;
    }

    fn set_error(&self, message: String) {
        let mut p = self.progress.lock();
        p.phase = "error".into();
        p.message = Some(message);
    }
}

// ===================== 版の取得 =====================

/// 実行ファイルの `--version` を読む。
pub async fn probe_version(command: &str) -> Option<YtdlpVersion> {
    let out = tools::tokio_command(command)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    YtdlpVersion::parse(&String::from_utf8_lossy(&out.stdout))
}

/// 同梱 / PATH 版 (= managed を除いた解決結果) を探す。
///
/// 「アプリを更新したら同梱版の方が新しくなった」を検出するために使う。
fn fallback_resolved(app: &tauri::AppHandle) -> tools::Resolved {
    tools::resolve_skipping_managed(Some(app), "yt-dlp")
}

// ===================== Tauri コマンド =====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YtdlpUpdateCheck {
    /// 追従チャンネル。
    pub channel: String,
    /// 今 yt-dlp として使われている実行ファイル。
    pub current_path: String,
    /// その版 (`--version` の実測値)。取れなければ `null`。
    pub current_version: Option<String>,
    /// 由来 (`managed` / `bundled` / `sidecar` / `system_path` / `not_found`)。
    pub current_source: String,
    /// リモートの最新版。オフライン等で取れなければ `null`。
    pub latest_version: Option<String>,
    /// リリースページ URL。
    pub release_url: Option<String>,
    /// 更新できるか (最新 > 現在)。
    pub update_available: bool,
    /// アプリが管理している版を入れているか。
    pub managed_installed: bool,
    /// 管理下バイナリのパス (入っていれば)。
    pub managed_path: Option<String>,
    /// 同梱 / PATH 版の方が新しいか (= 管理下の物を消した方が良い状態)。
    pub fallback_is_newer: bool,
    /// 同梱 / PATH 版の版。
    pub fallback_version: Option<String>,
    /// 最後にチェックした時刻 (Unix 秒)。
    pub last_checked_at: Option<i64>,
    /// 取得経路 (`api` / `redirect`)。診断用。
    pub via: Option<String>,
    /// 最新版を取れなかった理由。
    pub error: Option<String>,
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 設定からチャンネルを読む。引数が来ていればそちらを優先。
async fn resolve_channel(explicit: Option<String>, library: &LibraryHandle) -> UpdateChannel {
    if let Some(s) = explicit {
        return UpdateChannel::parse(&s);
    }
    let conn = library.lock().await;
    settings::get(&conn, KEY_CHANNEL)
        .ok()
        .flatten()
        .map(|s| UpdateChannel::parse(&s))
        .unwrap_or_default()
}

/// 現在の状態 + リモート最新版を調べる。
#[tauri::command]
pub async fn ytdlp_check_update(
    channel: Option<String>,
    library: State<'_, Arc<LibraryHandle>>,
    app: tauri::AppHandle,
) -> Result<YtdlpUpdateCheck> {
    let channel = resolve_channel(channel, &library).await;
    check_update_inner(channel, &library, &app).await
}

async fn check_update_inner(
    channel: UpdateChannel,
    library: &LibraryHandle,
    app: &tauri::AppHandle,
) -> Result<YtdlpUpdateCheck> {
    let current = tools::ytdlp(Some(app));
    let current_version = if matches!(current.source, tools::BinarySource::NotFound) {
        None
    } else {
        probe_version(&current.command).await
    };

    let managed = tools::managed_path(Some(app), "yt-dlp");
    let managed_installed = managed.as_ref().is_some_and(|p| p.is_file());

    // 同梱版が managed より新しいかは、managed が入っている時だけ意味を持つ。
    let fallback = fallback_resolved(app);
    let fallback_version =
        if managed_installed && !matches!(fallback.source, tools::BinarySource::NotFound) {
            probe_version(&fallback.command).await
        } else {
            None
        };
    let fallback_is_newer = match (&fallback_version, &current_version) {
        (Some(f), Some(c)) => f > c,
        _ => false,
    };

    let (latest, release_url, via, error) = match UpdateClient::new() {
        Ok(client) => match client.latest_release(channel).await {
            Ok(info) => (
                Some(info.version),
                Some(info.release_url),
                Some(info.via.to_string()),
                None,
            ),
            Err(e) => (None, None, None, Some(e.to_string())),
        },
        Err(e) => (None, None, None, Some(e.to_string())),
    };

    let update_available = match (&latest, &current_version) {
        (Some(l), Some(c)) => l > c,
        // 版が読めない (= yt-dlp が無い / 壊れている) なら入れる価値がある。
        (Some(_), None) => true,
        (None, _) => false,
    };

    let checked_at = now_unix();
    if let Some(l) = latest.as_ref() {
        let conn = library.lock().await;
        let _ = settings::set(&conn, KEY_LAST_CHECKED_AT, &checked_at.to_string());
        let _ = settings::set(&conn, KEY_LATEST_KNOWN, l.as_str());
    }

    Ok(YtdlpUpdateCheck {
        channel: channel.as_str().to_string(),
        current_path: current.command,
        current_version: current_version.map(|v| v.to_string()),
        current_source: current.source.as_str().to_string(),
        latest_version: latest.map(|v| v.to_string()),
        release_url,
        update_available,
        managed_installed,
        managed_path: managed.map(|p| p.to_string_lossy().into_owned()),
        fallback_is_newer,
        fallback_version: fallback_version.map(|v| v.to_string()),
        last_checked_at: Some(checked_at),
        via,
        error,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YtdlpInstallResult {
    /// 入れた版 (バイナリの `--version` 実測値)。
    pub version: String,
    pub path: String,
    pub bytes: u64,
    /// `SHA2-256SUMS` と照合できたか。
    pub sha256_verified: bool,
    pub channel: String,
}

/// 最新版を落として `<app_data_dir>/bin` に入れる。
#[tauri::command]
pub async fn ytdlp_install_update(
    channel: Option<String>,
    library: State<'_, Arc<LibraryHandle>>,
    state: State<'_, Arc<YtdlpUpdateState>>,
    tasks: State<'_, crate::commands::DownloadTasks>,
    app: tauri::AppHandle,
) -> Result<YtdlpInstallResult> {
    let channel = resolve_channel(channel, &library).await;
    let state = Arc::clone(&state);
    install_update_inner(channel, &library, &state, &tasks, &app).await
}

async fn install_update_inner(
    channel: UpdateChannel,
    library: &LibraryHandle,
    state: &Arc<YtdlpUpdateState>,
    tasks: &crate::commands::DownloadTasks,
    app: &tauri::AppHandle,
) -> Result<YtdlpInstallResult> {
    // 差し替え権をここで取る。二重インストール防止と「DL 中に差し替えない」
    // を 1 つのロックで同時に満たす。ガードを持っている間は新しい DL も
    // 弾かれるので、確認した後に DL が始まる隙間が無い。
    let Some(_swap) = tasks.try_begin_binary_swap() else {
        return Err(AppError::Other(
            "yt-dlp を更新できません。実行中のダウンロード、または別の更新の\
             完了を待ってからやり直してください。"
                .into(),
        ));
    };

    let result = install_update_run(channel, library, state, app).await;
    match &result {
        Ok(r) => {
            state.set(UpdateProgress {
                phase: "done".into(),
                downloaded_bytes: r.bytes,
                total_bytes: Some(r.bytes),
                version: Some(r.version.clone()),
                message: None,
            });
        }
        Err(e) => state.set_error(e.to_string()),
    }
    result
}

async fn install_update_run(
    channel: UpdateChannel,
    library: &LibraryHandle,
    state: &Arc<YtdlpUpdateState>,
    app: &tauri::AppHandle,
) -> Result<YtdlpInstallResult> {
    state.set(UpdateProgress {
        phase: "checking".into(),
        ..UpdateProgress::default()
    });

    let client = UpdateClient::new().map_err(AppError::from)?;
    let release = client
        .latest_release(channel)
        .await
        .map_err(AppError::from)?;
    let version_str = release.version.to_string();
    tracing::info!(
        version = %version_str,
        channel = channel.as_str(),
        via = release.via,
        url = %release.asset_url,
        "yt-dlp のアップデートを開始"
    );

    let dir = tools::managed_dir(Some(app))
        .ok_or_else(|| AppError::Other("app_data_dir を解決できませんでした".into()))?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(format!("{} を作成できません: {e}", dir.display())))?;
    let dest = dir.join(tools::exe_file_name("yt-dlp"));
    // 前回の中断で残った物を掃除しておく (残しても実害はないが容量を食う)。
    cleanup_temp_files(&dir).await;
    let tmp = dir.join(format!("{TEMP_PREFIX}{}", std::process::id()));

    state.set_phase("downloading", Some(&version_str));
    let expected = client.expected_sha256(&release).await;
    let downloaded = match client
        .download_asset(&release.asset_url, &tmp, |done, total| {
            state.set_bytes(done, total);
        })
        .await
    {
        Ok(d) => d,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::from(e));
        }
    };

    state.set_phase("verifying", Some(&version_str));
    let sha256_verified = match expected {
        Some(want) => {
            if want != downloaded.sha256 {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(AppError::Other(format!(
                    "yt-dlp {version_str} の SHA-256 が一致しません (期待 {want} / 実測 {})。\
                     ダウンロードが壊れているか、途中で差し替えられた可能性があります。",
                    downloaded.sha256
                )));
            }
            true
        }
        None => {
            tracing::warn!(
                version = %version_str,
                "SHA2-256SUMS を取得できなかったのでハッシュ照合をスキップした"
            );
            false
        }
    };

    // 実行ビットを立ててから `--version` を通す。ここまで通って初めて
    // 既存のバイナリを置き換える (壊れた DL で動く物を潰さない)。
    set_executable(&tmp).await?;
    let Some(installed_version) = probe_version(&tmp.to_string_lossy()).await else {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(AppError::Other(format!(
            "ダウンロードした yt-dlp {version_str} を実行できませんでした。\
             アーキテクチャが合っていないか、ファイルが壊れています。"
        )));
    };
    if installed_version != release.version {
        // GitHub の tag と中身が食い違うのは異常だが、実測値の方が真なので
        // そちらを採用して続行する。
        tracing::warn!(
            tag = %version_str,
            reported = %installed_version,
            "リリース tag と yt-dlp --version が食い違っている"
        );
    }

    state.set_phase("installing", Some(&installed_version.to_string()));
    tokio::fs::rename(&tmp, &dest)
        .await
        .map_err(|e| AppError::Other(format!("{} への設置に失敗しました: {e}", dest.display())))?;
    // rename でパーミッションは保たれるが、既存ファイルを置換した場合に
    // 備えてもう一度立てておく。
    set_executable(&dest).await?;

    {
        let conn = library.lock().await;
        let _ = settings::set(&conn, KEY_INSTALLED_VERSION, installed_version.as_str());
        let _ = settings::set(&conn, KEY_LATEST_KNOWN, release.version.as_str());
        let _ = settings::set(&conn, KEY_LAST_CHECKED_AT, &now_unix().to_string());
        let _ = settings::set(&conn, KEY_CHANNEL, channel.as_str());
    }
    // これを忘れると次の DL まで古いパスを掴んだままになる。
    tools::invalidate();

    tracing::info!(
        version = %installed_version,
        path = %dest.display(),
        bytes = downloaded.bytes,
        sha256_verified,
        "yt-dlp を更新した"
    );

    Ok(YtdlpInstallResult {
        version: installed_version.to_string(),
        path: dest.to_string_lossy().into_owned(),
        bytes: downloaded.bytes,
        sha256_verified,
        channel: channel.as_str().to_string(),
    })
}

/// インストールの進行状況。フロントがポーリングする。
#[tauri::command]
pub async fn ytdlp_update_status(
    state: State<'_, Arc<YtdlpUpdateState>>,
) -> Result<UpdateProgress> {
    Ok(state.snapshot())
}

/// 管理下の yt-dlp を消して同梱 / PATH 版に戻す。
#[tauri::command]
pub async fn ytdlp_remove_managed(
    library: State<'_, Arc<LibraryHandle>>,
    tasks: State<'_, crate::commands::DownloadTasks>,
    app: tauri::AppHandle,
) -> Result<bool> {
    // インストールと同じ差し替え権を取る。これを取らずに消すと、
    // 走っているインストールが rename した直後・成功を記録する前に消えて
    // 「成功したのに実行ファイルが無い」状態になったり、逆に消した直後に
    // インストールがファイルを作り直して「同梱版に戻した」が嘘になる
    // (Codex review P2)。
    let Some(_swap) = tasks.try_begin_binary_swap() else {
        return Err(AppError::Other(
            "yt-dlp を差し替えられません。実行中のダウンロード、または更新の\
             完了を待ってからやり直してください。"
                .into(),
        ));
    };
    let Some(path) = tools::managed_path(Some(&app), "yt-dlp") else {
        return Ok(false);
    };
    let existed = path.is_file();
    if existed {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| AppError::Other(format!("{} を削除できません: {e}", path.display())))?;
    }
    {
        let conn = library.lock().await;
        let _ = settings::delete(&conn, KEY_INSTALLED_VERSION);
    }
    tools::invalidate();
    tracing::info!(path = %path.display(), existed, "管理下の yt-dlp を削除した");
    Ok(existed)
}

// ===================== 起動時の自動チェック =====================

/// 起動直後に走らせる自動チェック。
///
/// - `ytdlp.auto_check` が false なら何もしない
/// - 前回チェックから 24h 経っていなければ何もしない
/// - `ytdlp.auto_install` が true で新版があればそのまま入れる
///
/// 失敗しても起動は止めない (ログだけ)。
pub fn spawn_auto_check(
    app: tauri::AppHandle,
    library: Arc<LibraryHandle>,
    state: Arc<YtdlpUpdateState>,
    tasks: crate::commands::DownloadTasks,
) {
    tauri::async_runtime::spawn(async move {
        // 起動直後はライブラリ読み込み等で忙しいので少し待つ。
        tokio::time::sleep(Duration::from_secs(5)).await;

        let (auto_check, auto_install, channel, last_checked) = {
            let conn = library.lock().await;
            let read = |key: &str| settings::get(&conn, key).ok().flatten();
            (
                // 既定 ON。明示的に "false" の時だけ止める。
                read(KEY_AUTO_CHECK).is_none_or(|v| v != "false"),
                read(KEY_AUTO_INSTALL).is_some_and(|v| v == "true"),
                read(KEY_CHANNEL)
                    .map(|s| UpdateChannel::parse(&s))
                    .unwrap_or_default(),
                read(KEY_LAST_CHECKED_AT).and_then(|v| v.parse::<i64>().ok()),
            )
        };
        if !auto_check {
            tracing::debug!("yt-dlp の自動アップデートチェックは無効");
            return;
        }
        if let Some(last) = last_checked {
            let elapsed = now_unix() - last;
            if (0..AUTO_CHECK_INTERVAL_SECS).contains(&elapsed) {
                tracing::debug!(elapsed, "yt-dlp の自動チェックはまだ間隔内");
                return;
            }
        }

        match check_update_inner(channel, &library, &app).await {
            Ok(check) => {
                if !check.update_available {
                    tracing::info!(
                        current = ?check.current_version,
                        latest = ?check.latest_version,
                        "yt-dlp は最新"
                    );
                    return;
                }
                tracing::info!(
                    current = ?check.current_version,
                    latest = ?check.latest_version,
                    auto_install,
                    "yt-dlp の新しい版がある"
                );
                if auto_install {
                    match install_update_inner(channel, &library, &state, &tasks, &app).await {
                        Ok(r) => tracing::info!(version = %r.version, "yt-dlp を自動更新した"),
                        Err(e) => tracing::warn!(error = %e, "yt-dlp の自動更新に失敗"),
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "yt-dlp の自動チェックに失敗"),
        }
    });
}

// ===================== 補助 =====================

async fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(path, perms).await.map_err(|e| {
            AppError::Other(format!(
                "{} に実行権限を付けられません: {e}",
                path.display()
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// 一時ファイル名の接頭辞。この後ろに PID が付く。
const TEMP_PREFIX: &str = "yt-dlp.download-";

/// 他プロセスの一時ファイルを「もう誰も書いていない」と見なす経過時間。
///
/// アップデートの DL は普通 1 分もかからないので、1 時間放置されている物は
/// 中断された残骸と判断してよい。
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// 中断で残った `yt-dlp.download-*` を掃除する。
///
/// 消すのは自分の PID の物か、十分に古い物だけ。差し替え権
/// ([`crate::commands::DownloadTasks::try_begin_binary_swap`]) はプロセス内の
/// 排他しか効かないので、アプリを 2 つ起動していると片方の掃除が、もう片方が
/// 今まさに書いている `yt-dlp.download-<相手の PID>` を消してしまう。Unix では
/// 書き込み自体は続くが、その後の chmod / rename がパス消失で失敗する
/// (Codex review P2)。
async fn cleanup_temp_files(dir: &Path) {
    let own = format!("{TEMP_PREFIX}{}", std::process::id());
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        // 自分の残骸は無条件に消してよい (このプロセスで書いている物は
        // 今から作る物だけで、それはまだ存在しない)。
        if name != own {
            // 他プロセスの物は、十分に古い場合だけ。mtime が読めない・
            // 未来の場合は触らない (安全側)。
            let fresh = entry
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age < STALE_TEMP_AGE);
            if fresh {
                continue;
            }
        }
        let _ = tokio::fs::remove_file(entry.path()).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---- バージョン ----

    #[test]
    fn parses_stable_version() {
        let v = YtdlpVersion::parse("2026.08.19").unwrap();
        assert_eq!(v.to_string(), "2026.08.19");
        assert!(!v.is_nightly());
    }

    #[test]
    fn parses_nightly_version() {
        let v = YtdlpVersion::parse("2026.08.20.234504").unwrap();
        assert_eq!(v.to_string(), "2026.08.20.234504");
        assert!(v.is_nightly());
    }

    #[test]
    fn parses_version_command_output() {
        // `yt-dlp --version` は改行付きで 1 行返す。dev ビルドは後ろに注記が付く。
        assert_eq!(
            YtdlpVersion::parse("2026.08.19\n").unwrap().to_string(),
            "2026.08.19"
        );
        assert_eq!(
            YtdlpVersion::parse("  2026.08.19 (dev)\n")
                .unwrap()
                .to_string(),
            "2026.08.19"
        );
        assert_eq!(
            YtdlpVersion::parse("v2026.08.19").unwrap().to_string(),
            "2026.08.19"
        );
    }

    #[test]
    fn rejects_malformed_versions() {
        for s in [
            "",
            "\n\n",
            "stable",
            "2026.08",        // 3 要素未満
            "2026.08.19.1.2", // 5 要素
            "2026..19",       // 空セグメント
            "2026.08.19-rc1", // 非数字
            "2026.08.1a",
        ] {
            assert!(YtdlpVersion::parse(s).is_none(), "should reject: {s:?}");
        }
    }

    #[test]
    fn orders_versions_by_date() {
        let older = YtdlpVersion::parse("2026.07.31").unwrap();
        let newer = YtdlpVersion::parse("2026.08.19").unwrap();
        assert!(newer > older);
        // ゼロ詰めの文字列比較だと 2026.10.01 < 2026.9.01 になりかねないので
        // 数値比較であることを確認する。
        let sep = YtdlpVersion::parse("2026.9.1").unwrap();
        let oct = YtdlpVersion::parse("2026.10.01").unwrap();
        assert!(oct > sep);
    }

    #[test]
    fn nightly_of_same_day_is_newer_than_stable() {
        let stable = YtdlpVersion::parse("2026.08.20").unwrap();
        let nightly = YtdlpVersion::parse("2026.08.20.234504").unwrap();
        assert!(nightly > stable);
        // 翌日の stable は前日の nightly より新しい。
        let next = YtdlpVersion::parse("2026.08.21").unwrap();
        assert!(next > nightly);
    }

    #[test]
    fn equality_ignores_zero_padding() {
        assert_eq!(
            YtdlpVersion::parse("2026.08.19").unwrap(),
            YtdlpVersion::parse("2026.8.19").unwrap()
        );
    }

    // ---- チャンネル ----

    #[test]
    fn channel_parsing_and_repos() {
        assert_eq!(UpdateChannel::parse("nightly"), UpdateChannel::Nightly);
        assert_eq!(UpdateChannel::parse("Nightly"), UpdateChannel::Nightly);
        assert_eq!(UpdateChannel::parse("stable"), UpdateChannel::Stable);
        // 未知の値・空文字は stable に倒す。
        assert_eq!(UpdateChannel::parse("beta"), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::parse(""), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::Stable.repo(), "yt-dlp/yt-dlp");
        assert_eq!(
            UpdateChannel::Nightly.repo(),
            "yt-dlp/yt-dlp-nightly-builds"
        );
    }

    // ---- 資産名 ----

    #[test]
    fn asset_name_matches_platform() {
        let name = asset_name();
        if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            assert_eq!(name, "yt-dlp_linux");
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            assert_eq!(name, "yt-dlp_linux_aarch64");
        } else if cfg!(target_os = "macos") {
            assert_eq!(name, "yt-dlp_macos");
        } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
            assert_eq!(name, "yt-dlp.exe");
        }
        // どのプラットフォームでも空にはならない。
        assert!(!name.is_empty());
    }

    // ---- URL / SUMS ----

    #[test]
    fn extracts_tag_from_redirect_location() {
        assert_eq!(
            tag_from_download_url(
                "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/SHA2-256SUMS"
            ),
            Some("2026.08.19")
        );
        assert_eq!(
            tag_from_download_url(
                "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/download/2026.08.20.234504/SHA2-256SUMS"
            ),
            Some("2026.08.20.234504")
        );
        assert_eq!(
            tag_from_download_url("https://github.com/yt-dlp/yt-dlp"),
            None
        );
        assert_eq!(
            tag_from_download_url("https://github.com/o/r/releases/download//SHA2-256SUMS"),
            None
        );
    }

    #[test]
    fn builds_download_urls() {
        assert_eq!(
            download_url(
                GITHUB_WEB_BASE,
                "yt-dlp/yt-dlp",
                "2026.08.19",
                "yt-dlp_linux"
            ),
            "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp_linux"
        );
        // 末尾スラッシュは with_bases が落とすので二重スラッシュにはならない。
        let client =
            UpdateClient::with_bases("https://api.example/", "https://web.example/").unwrap();
        assert_eq!(client.web_base, "https://web.example");
        assert_eq!(client.api_base, "https://api.example");
    }

    #[test]
    fn finds_sha256_for_asset() {
        // 実際の SHA2-256SUMS の一部。
        let body = "\
1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6  yt-dlp
58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a  yt-dlp_linux
b16e4dab368a816cd05d477d698a605a6ae87ccee1c8ffd38fa21d7254141fcc  yt-dlp_linux_aarch64
";
        assert_eq!(
            find_sha256(body, "yt-dlp_linux").as_deref(),
            Some("58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a")
        );
        // 前方一致で誤爆しないこと (`yt-dlp` が `yt-dlp_linux` を拾わない)。
        assert_eq!(
            find_sha256(body, "yt-dlp").as_deref(),
            Some("1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6")
        );
        assert!(find_sha256(body, "yt-dlp_macos").is_none());
    }

    #[test]
    fn ignores_malformed_sums_lines() {
        let body = "\
notahash  yt-dlp_linux
1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6
58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a  *yt-dlp_linux
";
        // 短すぎるハッシュ / 名前欠けは飛ばし、`*` 付き (バイナリモード) は拾う。
        assert_eq!(
            find_sha256(body, "yt-dlp_linux").as_deref(),
            Some("58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a")
        );
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn sha256_of_known_input() {
        // 空入力の SHA-256 (回帰用の固定値)。
        let digest = Sha256::digest([]);
        assert_eq!(
            hex_lower(&digest),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // ---- 進捗状態 ----

    #[test]
    fn progress_starts_idle() {
        let state = YtdlpUpdateState::default();
        let p = state.snapshot();
        assert_eq!(p.phase, "idle");
        assert_eq!(p.downloaded_bytes, 0);
        assert!(p.total_bytes.is_none());
    }

    #[test]
    fn progress_transitions_and_errors() {
        let state = YtdlpUpdateState::default();
        state.set_phase("downloading", Some("2026.08.19"));
        state.set_bytes(1024, Some(4096));
        let p = state.snapshot();
        assert_eq!(p.phase, "downloading");
        assert_eq!(p.version.as_deref(), Some("2026.08.19"));
        assert_eq!(p.downloaded_bytes, 1024);
        assert_eq!(p.total_bytes, Some(4096));

        state.set_error("boom".into());
        let p = state.snapshot();
        assert_eq!(p.phase, "error");
        assert_eq!(p.message.as_deref(), Some("boom"));
        // エラーでも直前の版・バイト数は残す (UI がどこで転けたか出せる)。
        assert_eq!(p.version.as_deref(), Some("2026.08.19"));
    }

    // ---- 一時ファイル掃除 ----

    /// `path` の mtime を `age` だけ過去にずらす。
    fn age_file(path: &Path, age: Duration) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        let when = std::time::SystemTime::now() - age;
        f.set_modified(when).unwrap();
    }

    #[tokio::test]
    async fn cleanup_removes_own_and_stale_temporaries_only() {
        let dir = tempfile::tempdir().unwrap();
        let keep_binary = dir.path().join("yt-dlp");
        let own = dir
            .path()
            .join(format!("{TEMP_PREFIX}{}", std::process::id()));
        // 別プロセスが「今まさに書いている」物。消してはいけない。
        let other_fresh = dir.path().join(format!("{TEMP_PREFIX}999999"));
        // 別プロセスが中断して置いていった物。消してよい。
        let other_stale = dir.path().join(format!("{TEMP_PREFIX}999998"));
        for p in [&keep_binary, &own, &other_fresh, &other_stale] {
            tokio::fs::write(p, b"x").await.unwrap();
        }
        age_file(&other_stale, STALE_TEMP_AGE + Duration::from_secs(60));

        cleanup_temp_files(dir.path()).await;

        assert!(keep_binary.exists(), "本体は消してはいけない");
        assert!(!own.exists(), "自分の残骸は消す");
        assert!(
            other_fresh.exists(),
            "他プロセスが書いている最中の物を消してはいけない"
        );
        assert!(!other_stale.exists(), "十分に古い残骸は消す");
    }

    #[tokio::test]
    async fn cleanup_on_missing_dir_is_noop() {
        // 存在しないディレクトリでも panic しないこと。
        cleanup_temp_files(Path::new("/nonexistent/nndd-test-dir")).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn set_executable_sets_mode_755() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin");
        tokio::fs::write(&p, b"#!/bin/sh\n").await.unwrap();
        set_executable(&p).await.unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[tokio::test]
    async fn probe_version_reads_stub_output() {
        // `--version` で版を吐くだけのスタブを作って読ませる。
        // (実物の yt-dlp が無い CI でも経路を確かめられる)
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("fake-ytdlp");
            tokio::fs::write(&p, "#!/bin/sh\necho 2026.08.19\n")
                .await
                .unwrap();
            set_executable(&p).await.unwrap();
            let v = probe_version(&p.to_string_lossy()).await.unwrap();
            assert_eq!(v.to_string(), "2026.08.19");
        }
    }

    // ---- ネットワーク経路 (mockito) ----

    /// GitHub API のレスポンス断片。`assets` は実物と同じ形。
    fn api_release_body(tag: &str, base: &str) -> String {
        let asset = asset_name();
        format!(
            r#"{{
              "tag_name": "{tag}",
              "html_url": "{base}/yt-dlp/yt-dlp/releases/tag/{tag}",
              "assets": [
                {{"name": "{asset}", "browser_download_url": "{base}/dl/{asset}"}},
                {{"name": "SHA2-256SUMS", "browser_download_url": "{base}/dl/SHA2-256SUMS"}}
              ]
            }}"#
        )
    }

    #[tokio::test]
    async fn latest_release_prefers_the_api() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let m = server
            .mock("GET", "/repos/yt-dlp/yt-dlp/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(api_release_body("2026.08.19", &base))
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let info = client.latest_release(UpdateChannel::Stable).await.unwrap();
        assert_eq!(info.version.to_string(), "2026.08.19");
        assert_eq!(info.via, "api");
        assert_eq!(info.asset_url, format!("{base}/dl/{}", asset_name()));
        assert_eq!(info.sums_url, format!("{base}/dl/SHA2-256SUMS"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn nightly_channel_hits_the_nightly_repo() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let m = server
            .mock("GET", "/repos/yt-dlp/yt-dlp-nightly-builds/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(api_release_body("2026.08.20.234504", &base))
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let info = client.latest_release(UpdateChannel::Nightly).await.unwrap();
        assert!(info.version.is_nightly());
        assert_eq!(info.version.to_string(), "2026.08.20.234504");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn falls_back_to_redirect_when_api_is_rate_limited() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        // API が 403 (レート制限 / 組織のプロキシで遮断) を返すケース。
        let api = server
            .mock("GET", "/repos/yt-dlp/yt-dlp/releases/latest")
            .with_status(403)
            .with_body(r#"{"message":"API rate limit exceeded"}"#)
            .create_async()
            .await;
        let redirect = server
            .mock(
                "GET",
                "/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            )
            .with_status(302)
            .with_header(
                "location",
                &format!("{base}/yt-dlp/yt-dlp/releases/download/2026.08.19/SHA2-256SUMS"),
            )
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let info = client.latest_release(UpdateChannel::Stable).await.unwrap();
        assert_eq!(info.version.to_string(), "2026.08.19");
        assert_eq!(info.via, "redirect");
        assert_eq!(
            info.asset_url,
            format!(
                "{base}/yt-dlp/yt-dlp/releases/download/2026.08.19/{}",
                asset_name()
            )
        );
        api.assert_async().await;
        redirect.assert_async().await;
    }

    #[tokio::test]
    async fn reports_both_failures_when_neither_path_works() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let _api = server
            .mock("GET", "/repos/yt-dlp/yt-dlp/releases/latest")
            .with_status(500)
            .create_async()
            .await;
        let _redirect = server
            .mock(
                "GET",
                "/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            )
            .with_status(404)
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let err = client
            .latest_release(UpdateChannel::Stable)
            .await
            .unwrap_err()
            .to_string();
        // 片方だけでなく両方の理由が出ること (切り分けできるように)。
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn rejects_api_release_without_our_asset() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let _api = server
            .mock("GET", "/repos/yt-dlp/yt-dlp/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            // この OS 向けの単体バイナリが無いリリース。
            .with_body(r#"{"tag_name":"2026.08.19","assets":[{"name":"other.zip","browser_download_url":"http://x/other.zip"}]}"#)
            .create_async()
            .await;
        // フォールバックも塞いでおく (API 側のエラーを確かめたいので)。
        let _redirect = server
            .mock(
                "GET",
                "/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            )
            .with_status(404)
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let err = client
            .latest_release(UpdateChannel::Stable)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(asset_name()), "{err}");
    }

    #[tokio::test]
    async fn downloads_asset_with_progress_and_hash() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let payload = vec![0xABu8; 300 * 1024]; // 128 KiB 刻みの進捗を数回踏ませる
        let _m = server
            .mock("GET", "/dl/asset")
            .with_status(200)
            .with_body(payload.clone())
            .create_async()
            .await;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("downloaded");
        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let mut samples: Vec<(u64, Option<u64>)> = Vec::new();
        let got = client
            .download_asset(&format!("{base}/dl/asset"), &dest, |done, total| {
                samples.push((done, total))
            })
            .await
            .unwrap();

        assert_eq!(got.bytes, payload.len() as u64);
        assert_eq!(got.sha256, hex_lower(&Sha256::digest(&payload)));
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), payload);
        // 最初は 0、最後は全量。単調非減少であること。
        assert_eq!(samples.first().map(|s| s.0), Some(0));
        assert_eq!(samples.last().map(|s| s.0), Some(payload.len() as u64));
        assert!(samples.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(samples.len() >= 3, "進捗が刻まれていない: {samples:?}");
    }

    #[tokio::test]
    async fn download_asset_surfaces_http_errors() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let _m = server
            .mock("GET", "/dl/missing")
            .with_status(404)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("downloaded");
        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let err = client
            .download_asset(&format!("{base}/dl/missing"), &dest, |_, _| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("404"), "{err}");
        // 失敗時に空ファイルを作り置きしないこと。
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn expected_sha256_reads_the_sums_asset() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let asset = asset_name();
        let want = "58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a";
        let _m = server
            .mock("GET", "/dl/SHA2-256SUMS")
            .with_status(200)
            .with_body(format!("{want}  {asset}\n"))
            .create_async()
            .await;

        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let release = ReleaseInfo {
            version: YtdlpVersion::parse("2026.08.19").unwrap(),
            asset_url: format!("{base}/dl/{asset}"),
            sums_url: format!("{base}/dl/SHA2-256SUMS"),
            release_url: base.clone(),
            via: "test",
        };
        assert_eq!(
            client.expected_sha256(&release).await.as_deref(),
            Some(want)
        );
    }

    #[tokio::test]
    async fn expected_sha256_is_none_when_sums_missing() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let _m = server
            .mock("GET", "/dl/SHA2-256SUMS")
            .with_status(404)
            .create_async()
            .await;
        let client = UpdateClient::with_bases(&base, &base).unwrap();
        let release = ReleaseInfo {
            version: YtdlpVersion::parse("2026.08.19").unwrap(),
            asset_url: format!("{base}/dl/x"),
            sums_url: format!("{base}/dl/SHA2-256SUMS"),
            release_url: base.clone(),
            via: "test",
        };
        // 取れないだけでは更新を止めない (照合をスキップして続行する契約)。
        assert!(client.expected_sha256(&release).await.is_none());
    }

    #[tokio::test]
    async fn probe_version_returns_none_on_failure() {
        let v = probe_version("/nonexistent/definitely-not-a-binary").await;
        assert!(v.is_none());
        #[cfg(unix)]
        {
            // 終了コード非 0 も None。
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("failing");
            tokio::fs::write(&p, "#!/bin/sh\nexit 1\n").await.unwrap();
            set_executable(&p).await.unwrap();
            assert!(probe_version(&p.to_string_lossy()).await.is_none());
        }
    }
}
