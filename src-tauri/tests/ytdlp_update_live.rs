//! yt-dlp アップデート追従の実ネットワーク検証。
//!
//! GitHub に実際に繋いで「最新版の解決 → SHA2-256SUMS の照合 → 資産 DL →
//! 実行権付与 → `--version` で動作確認」までを通す。モックでは踏めない
//! 実物のリダイレクト・資産名・ハッシュ形式を確かめるのが目的。
//!
//! 外部ネットワークに依存するので既定では走らせない (CI を不安定にしない)。
//! 手で回すときは:
//!
//! ```sh
//! cargo test --test ytdlp_update_live -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nndd_next_lib::downloader::ytdlp_update::{
    asset_name, probe_version, UpdateChannel, UpdateClient, YtdlpVersion,
};

/// stable / nightly の両方で「最新版が解決でき、資産 URL が組み立つ」こと。
#[tokio::test]
#[ignore = "外部ネットワーク (github.com) が要る"]
async fn resolves_latest_release_for_both_channels() {
    let client = UpdateClient::new().expect("client");

    for channel in [UpdateChannel::Stable, UpdateChannel::Nightly] {
        let info = client
            .latest_release(channel)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", channel.as_str()));
        println!(
            "[{}] version={} via={} asset={}",
            channel.as_str(),
            info.version,
            info.via,
            info.asset_url
        );

        // 資産 URL はこのプラットフォーム向けの名前で終わること。
        assert!(
            info.asset_url.ends_with(asset_name()),
            "asset_url が {} で終わっていない: {}",
            asset_name(),
            info.asset_url
        );
        assert!(info.sums_url.ends_with("SHA2-256SUMS"), "{}", info.sums_url);
        assert!(
            info.release_url.contains(channel.repo()),
            "{}",
            info.release_url
        );

        // nightly は 4 要素 (yyyy.mm.dd.HHMMSS)、stable は 3 要素。
        match channel {
            UpdateChannel::Nightly => assert!(
                info.version.is_nightly(),
                "nightly なのに 4 要素目が無い: {}",
                info.version
            ),
            UpdateChannel::Stable => assert!(
                !info.version.is_nightly(),
                "stable なのに 4 要素目がある: {}",
                info.version
            ),
        }
    }
}

/// 実物の `SHA2-256SUMS` から、このプラットフォーム向け資産のハッシュが引けること。
#[tokio::test]
#[ignore = "外部ネットワーク (github.com) が要る"]
async fn reads_the_real_sha256_sums() {
    let client = UpdateClient::new().expect("client");
    let info = client
        .latest_release(UpdateChannel::Stable)
        .await
        .expect("latest");
    let hash = client
        .expected_sha256(&info)
        .await
        .expect("SHA2-256SUMS に自分の資産が載っているはず");
    println!("expected sha256({}) = {hash}", asset_name());
    assert_eq!(hash.len(), 64);
    assert!(hash
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}

/// インストール経路まるごと: DL → ハッシュ照合 → chmod → `--version`。
///
/// `install_update_run` は Tauri の `AppHandle` を要求するのでここでは呼べない。
/// その中身と同じ順序を手で組んで、実物のバイナリで通ることを確かめる。
#[tokio::test]
#[ignore = "外部ネットワーク (github.com) が要る / 数十 MB DL する"]
async fn downloads_verifies_and_runs_the_real_binary() {
    let client = UpdateClient::new().expect("client");
    let info = client
        .latest_release(UpdateChannel::Stable)
        .await
        .expect("latest");
    let expected = client.expected_sha256(&info).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join(asset_name());

    let mut last = 0u64;
    let got = client
        .download_asset(&info.asset_url, &dest, |done, total| {
            // 進捗が単調に増えること (0 から始まり戻らない)。
            assert!(done >= last, "進捗が巻き戻った: {last} -> {done}");
            last = done;
            if let Some(t) = total {
                assert!(done <= t, "総量を超えた: {done} > {t}");
            }
        })
        .await
        .expect("download");
    println!(
        "downloaded {} bytes, sha256={} (expected={:?})",
        got.bytes, got.sha256, expected
    );

    assert!(got.bytes > 1_000_000, "小さすぎる: {} bytes", got.bytes);
    assert_eq!(
        got.bytes,
        std::fs::metadata(&dest).expect("stat").len(),
        "書き出したファイルのサイズが DL 量と合わない"
    );
    if let Some(want) = expected {
        assert_eq!(want, got.sha256, "リリース同梱のハッシュと一致しない");
    }

    // 実行権を立てて動くことを確認する (install_update_run と同じ順序)。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let reported = probe_version(&dest.to_string_lossy())
        .await
        .expect("DL した yt-dlp が --version を返すこと");
    println!("binary reports version {reported}");
    assert_eq!(
        reported, info.version,
        "リリース tag と yt-dlp --version が食い違っている"
    );
}

/// 実物の tag が `YtdlpVersion` で読めて、古い版より新しいと判定されること。
#[tokio::test]
#[ignore = "外部ネットワーク (github.com) が要る"]
async fn latest_is_newer_than_an_old_release() {
    let client = UpdateClient::new().expect("client");
    let latest = client
        .latest_release(UpdateChannel::Stable)
        .await
        .expect("latest")
        .version;
    // yt-dlp の初期リリース近辺。これより古くなることはない。
    let ancient = YtdlpVersion::parse("2021.01.08").expect("parse");
    assert!(latest > ancient, "{latest} <= {ancient}");
}
