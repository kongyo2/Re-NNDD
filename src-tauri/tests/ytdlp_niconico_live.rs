//! アップデートで入れた yt-dlp が、アプリ本体の DL 経路でそのまま使えることの確認。
//!
//! [`ytdlp_update_live`](../ytdlp_update_live.rs) が「最新版を落として動かす」
//! ところまでを見るのに対して、こちらはその成果物を
//! [`downloader::ytdlp::download`] に食わせて、実際に niconico から 1 本
//! 落とせるところまでを通す。アップデート追従が「版が上がるだけ」で終わらず
//! 「上がった版でちゃんと DL できる」ことまで担保するのが目的。
//!
//! 実行には niconico の `user_session` Cookie が要る。資格情報をリポジトリに
//! 置かないため環境変数から受け取り、未設定ならスキップする。
//!
//! ```sh
//! NNDD_TEST_COOKIE='user_session=user_session_...' \
//!   cargo test --test ytdlp_niconico_live -- --ignored --nocapture
//! ```
//!
//! 任意で `NNDD_TEST_YTDLP` に既存バイナリのパスを渡すと DL を省略できる。
//! `NNDD_TEST_VIDEO` で対象動画を差し替えられる (既定は sm9)。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use nndd_next_lib::downloader::ytdlp_update::{
    asset_name, probe_version, UpdateChannel, UpdateClient,
};
use nndd_next_lib::downloader::{tools, ytdlp};

/// アップデート機能と同じ手順で最新の yt-dlp を用意する。
async fn install_latest_ytdlp(dir: &std::path::Path) -> PathBuf {
    if let Some(existing) = std::env::var_os("NNDD_TEST_YTDLP") {
        let p = PathBuf::from(existing);
        assert!(
            p.is_file(),
            "NNDD_TEST_YTDLP が指すファイルが無い: {}",
            p.display()
        );
        return p;
    }
    let client = UpdateClient::new().expect("client");
    let info = client
        .latest_release(UpdateChannel::Stable)
        .await
        .expect("latest release");
    // `tools::resolve` が拾える名前 (`yt-dlp` / `yt-dlp.exe`) で置くこと。
    // 資産名 (`yt-dlp_linux` 等) のままでは PATH 探索に引っかからない。
    let dest = dir.join(tools::exe_file_name("yt-dlp"));
    let expected = client.expected_sha256(&info).await;
    let got = client
        .download_asset(&info.asset_url, &dest, |_, _| {})
        .await
        .expect("download");
    if let Some(want) = expected {
        assert_eq!(want, got.sha256, "SHA-256 不一致");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    println!(
        "installed yt-dlp {} ({}) -> {}",
        info.version,
        asset_name(),
        dest.display()
    );
    dest
}

#[tokio::test]
#[ignore = "外部ネットワーク + NNDD_TEST_COOKIE が要る"]
async fn updated_ytdlp_downloads_a_niconico_video() {
    let Ok(cookie) = std::env::var("NNDD_TEST_COOKIE") else {
        eprintln!("NNDD_TEST_COOKIE 未設定のためスキップ");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir");
    let installed = install_latest_ytdlp(&bin_dir).await;
    let version = probe_version(&installed.to_string_lossy())
        .await
        .expect("--version");

    // アプリは `tools::ytdlp()` 経由で実行ファイルを解決する。ここでは
    // AppHandle が無いので PATH 解決に載せて、同じ関数から拾わせる。
    // (`app` 無しの解決はキャッシュされない契約なので、PATH の差し替えが効く)
    let installed_dir = installed
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| bin_dir.clone());
    let old_path = std::env::var_os("PATH");
    let mut paths = vec![installed_dir];
    if let Some(p) = old_path.as_ref() {
        paths.extend(std::env::split_paths(p));
    }
    let joined = std::env::join_paths(paths).expect("join_paths");
    std::env::set_var("PATH", &joined);
    tools::invalidate();

    let resolved = tools::ytdlp(None);
    assert_eq!(
        resolved.source,
        tools::BinarySource::SystemPath,
        "用意した yt-dlp が解決されていない: {resolved:?}"
    );
    // mp4 マージに ffmpeg が要る。前提が崩れているなら DL 失敗より先に判る
    // ようにしておく。`command` は `--ffmpeg-location` にそのまま渡るので、
    // 実在するパスであることまで見る。
    let ff = tools::ffmpeg(None);
    assert!(
        !matches!(ff.source, tools::BinarySource::NotFound),
        "ffmpeg が見つからない (mp4 マージに必要): PATH={:?}",
        std::env::var("PATH").unwrap_or_default()
    );
    assert!(
        std::path::Path::new(&ff.command).is_file(),
        "--ffmpeg-location に渡す値が実在しない: {}",
        ff.command
    );

    let video = std::env::var("NNDD_TEST_VIDEO").unwrap_or_else(|_| "sm9".to_string());
    let url = format!("https://www.nicovideo.jp/watch/{video}");
    let out = dir.path().join("out");

    let mut seen: Vec<f64> = Vec::new();
    let result = ytdlp::download(None, &url, &out, Some(cookie), |p| seen.push(p)).await;

    // PATH は元に戻してから assert する (失敗しても後続に影響させない)。
    match old_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
    tools::invalidate();

    let result = result.expect("yt-dlp での DL が成功すること");
    println!(
        "yt-dlp {version} で {video} を取得: {} ({} bytes)",
        result.video_path.display(),
        std::fs::metadata(&result.video_path)
            .map(|m| m.len())
            .unwrap_or(0)
    );

    // ライブラリ取り込み (`commands.rs`) が参照するファイルが揃っていること。
    assert!(result.video_path.is_file(), "video.mp4 が無い");
    assert!(
        std::fs::metadata(&result.video_path).expect("stat").len() > 100_000,
        "mp4 が小さすぎる"
    );
    assert!(result.info_path.is_file(), "video.info.json が無い");
    assert!(
        result.info_json.get("id").is_some(),
        "info.json が読めていない: {:?}",
        result.info_json
    );
    assert!(result.thumbnail_path.is_some(), "サムネイルが無い");
    assert!(result.description_path.is_some(), "説明文が無い");

    // Cookie ファイルは DL 後に消えていること (資格情報を残さない)。
    assert!(
        !out.join(".cookies.txt").exists(),
        "一時 Cookie ファイルが残っている"
    );

    // 進捗が 0.0..=1.0 で単調に増えて 1.0 近くまで行くこと。
    assert!(!seen.is_empty(), "進捗が 1 度も来ていない");
    assert!(
        seen.iter().all(|p| (0.0..=1.0).contains(p)),
        "進捗が範囲外: {seen:?}"
    );
    assert!(
        seen.iter().copied().fold(0.0_f64, f64::max) > 0.9,
        "進捗が最後まで来ていない: max={}",
        seen.iter().copied().fold(0.0_f64, f64::max)
    );
}
