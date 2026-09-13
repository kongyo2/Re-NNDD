//! バンドル / システムどちらの yt-dlp / ffmpeg を使うかの解決。
//!
//! アプリ自身が DL してきた「管理下」バイナリを最優先。次に Tauri の
//! `bundle.externalBin` でアプリにバンドルされたバイナリ。無ければ
//! システム PATH にフォールバック。
//!
//! 解決順:
//! 1. `<app_data_dir>/bin/yt-dlp[.exe]` (アップデート機能が置く managed バイナリ)
//! 2. `<resource_dir>/yt-dlp[.exe]` (Tauri bundle 配置先)
//! 3. `<exe_dir>/yt-dlp[.exe]`
//! 4. `<exe_dir>/binaries/yt-dlp[.exe]` (dev `cargo tauri dev` で隣に置かれる場合)
//! 5. `<src-tauri>/binaries/yt-dlp-<triple>[.exe]` (cargo run で workspace ルートから動かしたケース)
//! 6. PATH 検索 (`yt-dlp`)
//!
//! `Resolved::source` を使えばどこから取れたか UI に出せる。
//!
//! 解決結果はプロセス内でキャッシュするが、アップデートで managed バイナリが
//! 増減すると答えが変わるので [`invalidate`] で捨てられるようにしてある
//! (`OnceLock` だと再起動するまで古い解決結果に張り付いてしまう)。

use std::path::PathBuf;

use parking_lot::RwLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinarySource {
    /// アプリが DL・管理している `<app_data_dir>/bin` 配下のバイナリ
    Managed,
    /// Tauri バンドルされたサイドカー
    Bundled,
    /// 実行ファイル隣 / dev 用 binaries フォルダ
    Sidecar,
    /// システム PATH
    SystemPath,
    /// 見つからなかった
    NotFound,
}

impl BinarySource {
    /// フロント (`AppInfo.ytdlpSource` 等) に出す安定した識別子。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Bundled => "bundled",
            Self::Sidecar => "sidecar",
            Self::SystemPath => "system_path",
            Self::NotFound => "not_found",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    /// 実際に呼び出すコマンド。
    ///
    /// `source` が `NotFound` 以外なら **実在する絶対パス**。spawn するだけなら
    /// 裸の名前でも PATH が引かれるが、この値は yt-dlp の `--ffmpeg-location`
    /// にもそのまま渡すので、実在するパスであることが要る。
    pub command: String,
    pub source: BinarySource,
}

impl Resolved {
    pub fn not_found(name: &str) -> Self {
        Self {
            command: name.to_string(),
            source: BinarySource::NotFound,
        }
    }
}

/// アプリが管理する（= アップデート機能が書き込む）バイナリの置き場。
///
/// `<app_data_dir>/bin`。`app_data_dir` は Tauri が dev/prod と OS ごとに
/// 出し分けるので、開発ビルドが本番プロファイルの yt-dlp を上書きすることはない。
pub fn managed_dir(app: Option<&tauri::AppHandle>) -> Option<PathBuf> {
    use tauri::Manager;
    let app = app?;
    app.path().app_data_dir().ok().map(|d| d.join("bin"))
}

/// 管理下バイナリのフルパス（存在するかは問わない）。
pub fn managed_path(app: Option<&tauri::AppHandle>, name: &str) -> Option<PathBuf> {
    Some(managed_dir(app)?.join(exe_file_name(name)))
}

/// OS ごとの実行ファイル名 (`yt-dlp` / `yt-dlp.exe`)。
pub fn exe_file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// `app` (Tauri AppHandle) を渡せばバンドル resource_dir も探す。
/// `None` なら exe_dir 周辺と PATH のみ。
pub fn resolve(app: Option<&tauri::AppHandle>, name: &str) -> Resolved {
    // 0) <app_data_dir>/bin/yt-dlp(.exe)
    //    ユーザーが明示的にアップデートを実行して置いた物なので、バンドル版より
    //    優先する。同梱版の方が新しくなった場合の扱いは ytdlp_update 側が
    //    「同梱版に戻す」導線として面倒を見る。
    if let Some(cand) = managed_path(app, name) {
        if cand.is_file() {
            return Resolved {
                command: cand.to_string_lossy().into_owned(),
                source: BinarySource::Managed,
            };
        }
    }
    resolve_skipping_managed(app, name)
}

/// managed バイナリを無視した解決。
///
/// 「アップデートで入れた版を消したら何に落ちるか」「同梱版の方が新しく
/// なっていないか」を [`crate::downloader::ytdlp_update`] が調べるのに使う。
pub fn resolve_skipping_managed(app: Option<&tauri::AppHandle>, name: &str) -> Resolved {
    let exe_name = exe_file_name(name);

    // 1) resource_dir/yt-dlp(.exe)
    if let Some(app) = app {
        use tauri::Manager;
        if let Ok(resource_dir) = app.path().resource_dir() {
            let candidate = resource_dir.join(&exe_name);
            if candidate.is_file() {
                return Resolved {
                    command: candidate.to_string_lossy().into_owned(),
                    source: BinarySource::Bundled,
                };
            }
        }
    }

    // 2) <exe_dir>/yt-dlp
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(&exe_name);
            if cand.is_file() {
                return Resolved {
                    command: cand.to_string_lossy().into_owned(),
                    source: BinarySource::Sidecar,
                };
            }
            // 3) <exe_dir>/binaries/yt-dlp
            let cand2 = dir.join("binaries").join(&exe_name);
            if cand2.is_file() {
                return Resolved {
                    command: cand2.to_string_lossy().into_owned(),
                    source: BinarySource::Sidecar,
                };
            }
            // 4) workspace root の binaries/<name>-<triple> も探す (cargo run 用)
            //    target/<profile>/<exe> から 2 つ上が workspace ルート想定。
            if let Some(workspace) = dir.parent().and_then(|p| p.parent()) {
                if let Some(cand3) = find_with_triple_suffix(&workspace.join("binaries"), name) {
                    return Resolved {
                        command: cand3.to_string_lossy().into_owned(),
                        source: BinarySource::Sidecar,
                    };
                }
                // src-tauri/binaries/<name>-<triple> も
                if let Some(cand4) =
                    find_with_triple_suffix(&workspace.join("src-tauri").join("binaries"), name)
                {
                    return Resolved {
                        command: cand4.to_string_lossy().into_owned(),
                        source: BinarySource::Sidecar,
                    };
                }
            }
        }
    }

    // 5) PATH
    //
    // 見つかった **絶対パス** を返すこと。`"ffmpeg"` のような裸の名前でも
    // `Command::new` は PATH を引いてくれるが、`command` は yt-dlp の
    // `--ffmpeg-location` にもそのまま渡している。yt-dlp はこの値を
    // 「実在するパス」として扱い、存在しなければ ffmpeg 無しで続行して
    // マージ時に `Postprocessing: ffmpeg not found` で落ちる
    // (`--no-warnings` を付けているので警告も出ない)。
    // 同梱もサイドカーも無く PATH の ffmpeg だけがある環境で、DL が
    // 必ず失敗していた。
    if let Some(found) = which_in_path(name) {
        return Resolved {
            command: found.to_string_lossy().into_owned(),
            source: BinarySource::SystemPath,
        };
    }

    Resolved::not_found(name)
}

fn find_with_triple_suffix(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let prefix = format!("{name}-");
    for e in entries.flatten() {
        let fname = e.file_name();
        let s = fname.to_string_lossy();
        if s.starts_with(&prefix) {
            let p = e.path();
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(&exe_name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

// ====== キャッシュ ======
// 起動中に何度も解決しないでよいように一度結果を取っておく。
// アップデートで managed バイナリが増減したら `invalidate()` で捨てる。
//
// 毒化 (poisoning) を気にせず読み書きしたいので parking_lot を使う。
// `unwrap_used` / `expect_used` が deny なので std の RwLock だと
// 毎回 match を書く羽目になる。
static YTDLP_CACHE: RwLock<Option<Resolved>> = RwLock::new(None);
static FFMPEG_CACHE: RwLock<Option<Resolved>> = RwLock::new(None);

fn cached(
    cache: &'static RwLock<Option<Resolved>>,
    app: Option<&tauri::AppHandle>,
    name: &str,
) -> Resolved {
    if let Some(hit) = cache.read().clone() {
        return hit;
    }
    let resolved = resolve(app, name);
    // `app` 無しの解決は managed / bundled を見られないので不完全。これを
    // キャッシュに焼くと、以降 AppHandle 付きで呼んでも PATH 版に張り付く。
    // 完全な解決 (= AppHandle あり) の結果だけをキャッシュする。
    if app.is_some() {
        // 解決自体は数回の stat なので、write ロックを取ってから再確認する
        // (二重解決しても結果は同じなので厳密な排他は要らないが、書き込みを
        //  1 回に抑えておく)。
        let mut guard = cache.write();
        if let Some(hit) = guard.clone() {
            return hit;
        }
        *guard = Some(resolved.clone());
    }
    resolved
}

pub fn ytdlp(app: Option<&tauri::AppHandle>) -> Resolved {
    cached(&YTDLP_CACHE, app, "yt-dlp")
}

pub fn ffmpeg(app: Option<&tauri::AppHandle>) -> Resolved {
    cached(&FFMPEG_CACHE, app, "ffmpeg")
}

/// 解決結果のキャッシュを捨てる。
///
/// yt-dlp をアップデート / 削除した直後に呼ぶこと。これを忘れると
/// アプリを再起動するまで古いパスを掴んだままになる。
pub fn invalidate() {
    *YTDLP_CACHE.write() = None;
    *FFMPEG_CACHE.write() = None;
}

// ====== サブプロセス起動ヘルパ ======
// Windows では GUI アプリから素の `Command::new(...).output()` を呼ぶと
// 子プロセスごとにコンソールウィンドウが一瞬チラつく。設定画面の
// `get_app_info` は yt-dlp / ffmpeg の `--version` を毎回 2 本叩くため、
// 開くたびにターミナルが立ち上がりまくる挙動になっていた。
// `CREATE_NO_WINDOW` を付ければウィンドウを作らずに起動できる。Unix では no-op。

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `tokio::process::Command::new` の代わりに使う。Windows ではコンソール
/// ウィンドウを抑制するフラグを立てる。
pub fn tokio_command<S: AsRef<std::ffi::OsStr>>(program: S) -> tokio::process::Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// `std::process::Command::new` の代わりに使う。Windows ではコンソール
/// ウィンドウを抑制するフラグを立てる。
pub fn std_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn exe_file_name_follows_the_platform() {
        let name = exe_file_name("yt-dlp");
        if cfg!(windows) {
            assert_eq!(name, "yt-dlp.exe");
        } else {
            assert_eq!(name, "yt-dlp");
        }
    }

    #[test]
    fn binary_source_identifiers_are_stable() {
        // フロント (`AppInfo.ytdlpSource` / `YtdlpUpdateCheck.currentSource`) が
        // この文字列で分岐するので、勝手に変えると UI が壊れる。
        assert_eq!(BinarySource::Managed.as_str(), "managed");
        assert_eq!(BinarySource::Bundled.as_str(), "bundled");
        assert_eq!(BinarySource::Sidecar.as_str(), "sidecar");
        assert_eq!(BinarySource::SystemPath.as_str(), "system_path");
        assert_eq!(BinarySource::NotFound.as_str(), "not_found");
    }

    #[test]
    fn not_found_keeps_the_bare_name_as_command() {
        let r = Resolved::not_found("yt-dlp");
        assert_eq!(r.command, "yt-dlp");
        assert_eq!(r.source, BinarySource::NotFound);
    }

    #[test]
    fn managed_paths_need_an_app_handle() {
        // AppHandle 無しでは app_data_dir が判らないので managed は諦める
        // (= `resolve(None, ..)` は managed を返さない、の裏付け)。
        assert!(managed_dir(None).is_none());
        assert!(managed_path(None, "yt-dlp").is_none());
    }

    #[test]
    fn resolve_without_app_never_reports_managed() {
        let r = resolve(None, "definitely-not-a-real-binary-name-xyz");
        assert_eq!(r.source, BinarySource::NotFound);
        // skip 版も同じ答えになること (managed 判定以外は共通経路)。
        let s = resolve_skipping_managed(None, "definitely-not-a-real-binary-name-xyz");
        assert_eq!(s.source, BinarySource::NotFound);
    }

    #[test]
    fn resolve_finds_binaries_on_path() {
        // どの CI にも居る実行ファイルで PATH 探索の経路を通す。
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        let r = resolve(None, name);
        assert_eq!(r.source, BinarySource::SystemPath);
        // 裸の名前ではなく絶対パスを返すこと。`command` は yt-dlp の
        // `--ffmpeg-location` にも渡るので、実在するパスでないと
        // マージ時に "ffmpeg not found" で落ちる (回帰防止)。
        let path = std::path::Path::new(&r.command);
        assert!(path.is_absolute(), "絶対パスであること: {}", r.command);
        assert!(path.is_file(), "実在するファイルであること: {}", r.command);
        assert_eq!(
            path.file_name().map(|s| s.to_string_lossy().into_owned()),
            Some(exe_file_name(name))
        );
    }

    #[test]
    fn invalidate_clears_the_cache() {
        // `app` 無しの解決はキャッシュに載らない契約なので、invalidate 前後で
        // 素直に再解決される (= 古い答えに張り付かない)。
        invalidate();
        let first = ytdlp(None);
        invalidate();
        let second = ytdlp(None);
        assert_eq!(first.source, second.source);
        assert_eq!(first.command, second.command);
    }
}
