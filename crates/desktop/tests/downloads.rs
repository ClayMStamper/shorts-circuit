//! Originals downloaded from TikTok: a video in the downloads folder is attached to the
//! pending TikTok post it is the original of ("Get original" makes an older post win), and
//! anything else there is left alone. The fake media backend reports 20 s for every file.

mod support;

use std::time::Duration;

use chrono::Duration as ChronoDuration;
use shorts_desktop::watch::{WatchConfig, WatchFolder, WatchKind};
use shorts_desktop::{AppEvent, LibraryQuery};
use shorts_domain::{Asset, AssetId, AssetStatus, TikTokPost};
use support::{Harness, harness, t0};

/// A detected TikTok post waiting for its original file.
async fn pending_post(h: &Harness, video_id: &str, hours_ago: i64, duration_ms: u64) -> AssetId {
    let posted_at = t0() - ChronoDuration::hours(hours_ago);
    let url = format!("https://www.tiktok.com/@timeless_way_interactive/video/{video_id}");
    let mut asset = Asset::awaiting_media(&format!("Post {video_id}"), Some(&url), t0()).unwrap();
    asset.notes = format!("Caption of {video_id} #gamedev");
    let post = TikTokPost {
        video_id: video_id.into(),
        asset_id: asset.id,
        posted_at,
        duration_ms: Some(duration_ms),
        title: asset.title.clone(),
        caption: asset.notes.clone(),
        share_url: Some(url),
        cover_url: None,
        cover_path: None,
        discovered_at: t0(),
        matched_at: None,
    };
    assert!(
        h.app
            .store()
            .record_tiktok_post(&asset, &post)
            .await
            .unwrap()
    );
    asset.id
}

fn downloads(h: &Harness) -> std::path::PathBuf {
    let dir = h.dir.path().join("Downloads");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn activity_item(h: &Harness, id: AssetId) -> shorts_desktop::views::RuleItemView {
    h.app
        .activity(50)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.asset_id == id.to_string())
        .expect("the item is in Activity")
}

async fn library_count(h: &Harness) -> usize {
    h.app
        .list_assets(&LibraryQuery::default())
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_download_is_attached_to_the_post_it_fits() {
    let h = harness().await;
    let post = pending_post(&h, "7001", 3, 20_000).await;
    let file = downloads(&h).join("7001.mp4");
    std::fs::write(&file, b"tiktok original").unwrap();

    let attached = h.app.import_download(&file).await.unwrap();
    assert_eq!(attached, Some(post));
    let asset = h.app.store().assets().require(post).await.unwrap();
    assert_eq!(asset.status, AssetStatus::Ready);
    assert_eq!(
        asset.notes, "Caption of 7001 #gamedev",
        "the post's caption is kept"
    );
    let tiktok = h
        .app
        .store()
        .tiktok_posts()
        .get("7001")
        .await
        .unwrap()
        .unwrap();
    assert!(tiktok.matched_at.is_some());
    assert!(
        h.app
            .store()
            .tiktok_posts()
            .pending()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unrelated_and_ambiguous_downloads_are_left_alone() {
    let h = harness().await;
    pending_post(&h, "7002", 3, 50_000).await;
    let before = library_count(&h).await;
    let file = downloads(&h).join("holiday.mp4");
    std::fs::write(&file, b"some other video").unwrap();
    assert_eq!(h.app.import_download(&file).await.unwrap(), None);

    // Two recent posts of the same length: ambiguous without "Get original".
    pending_post(&h, "7003", 2, 20_000).await;
    pending_post(&h, "7004", 1, 20_000).await;
    assert_eq!(h.app.import_download(&file).await.unwrap(), None);
    assert_eq!(library_count(&h).await, before + 2, "nothing was imported");
    assert!(file.is_file(), "the download stays where it is");
}

#[tokio::test]
async fn get_original_makes_an_older_post_win() {
    let h = harness().await;
    // Posted three weeks ago: too old for automatic matching.
    let old = pending_post(&h, "7005", 24 * 21, 20_000).await;
    let recent = pending_post(&h, "7006", 1, 20_000).await;
    let url = h.app.expect_original(old).await.unwrap();
    assert_eq!(
        url,
        "https://www.tiktok.com/@timeless_way_interactive/video/7005"
    );

    let file = downloads(&h).join("7005.mp4");
    std::fs::write(&file, b"the old one").unwrap();
    assert_eq!(h.app.import_download(&file).await.unwrap(), Some(old));
    assert_eq!(
        h.app.store().assets().require(recent).await.unwrap().status,
        AssetStatus::AwaitingMedia
    );

    // An item that already has its video can't be asked for again.
    assert!(h.app.expect_original(old).await.is_err());
}

#[tokio::test]
async fn the_downloads_watch_only_takes_new_files_that_fit() {
    let h = harness().await;
    let post = pending_post(&h, "7007", 2, 20_000).await;
    let dir = downloads(&h);
    // Already there before watching starts, and old: never swept in.
    let old = dir.join("old.mp4");
    std::fs::write(&old, b"from last month").unwrap();
    let month_ago = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(month_ago)
        .unwrap();

    let mut rx = h.app.subscribe();
    let _watch = WatchFolder::start(
        h.app.clone(),
        dir.clone(),
        WatchConfig {
            settle: Duration::from_millis(200),
            poll: Duration::from_millis(50),
            rescan: Duration::from_secs(3600),
            kind: WatchKind::Downloads,
        },
    )
    .unwrap();
    std::fs::write(dir.join("partial.mp4.crdownload"), b"still downloading").unwrap();
    std::fs::write(dir.join("7007.mp4"), b"the new download").unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let AppEvent::Imported { asset_id, .. } = rx.recv().await.unwrap() {
                assert_eq!(asset_id, post.to_string());
                return;
            }
        }
    })
    .await
    .expect("the download was never attached");
    assert_eq!(
        h.app.store().assets().require(post).await.unwrap().status,
        AssetStatus::Ready
    );
    assert_eq!(library_count(&h).await, 1, "the old file was not imported");
}

#[tokio::test]
async fn waiting_status_notes_and_cancel() {
    let h = harness().await;
    // 30 s post; the fake media backend reports 20 s for any file.
    let post = pending_post(&h, "7010", 1, 30_000).await;
    h.app.expect_original(post).await.unwrap();
    let file = downloads(&h).join("Download (3).mp4");
    std::fs::write(&file, b"a different video").unwrap();
    assert_eq!(h.app.import_download(&file).await.unwrap(), None);

    let item = activity_item(&h, post).await;
    let until = item.expecting_original_until.expect("still waiting");
    assert_eq!(until, t0() + ChronoDuration::minutes(30));
    let note = item
        .download_note
        .expect("the skipped download is explained");
    assert!(
        note.contains("Download (3).mp4") && note.contains("20 s") && note.contains("30 s"),
        "{note}"
    );

    h.app.cancel_expect_original(post).await.unwrap();
    let item = activity_item(&h, post).await;
    assert!(item.expecting_original_until.is_none() && item.download_note.is_none());
}

#[tokio::test]
async fn pick_file_attaches_any_video_and_announces_it() {
    let h = harness().await;
    // The picked file's length doesn't matter: the owner chose it.
    let post = pending_post(&h, "7011", 24 * 30, 90_000).await;
    let mut rx = h.app.subscribe();
    let file = downloads(&h).join("Download (2).mp4");
    std::fs::write(&file, b"picked by hand").unwrap();
    assert_eq!(h.app.attach_original(post, &file).await.unwrap(), post);
    let asset = h.app.store().assets().require(post).await.unwrap();
    assert_eq!(asset.status, AssetStatus::Ready);
    assert!(
        h.app
            .store()
            .tiktok_posts()
            .pending()
            .await
            .unwrap()
            .is_empty()
    );
    let announced = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AppEvent::OriginalAttached { asset_id, title } = rx.recv().await.unwrap() {
                return (asset_id, title);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(announced, (post.to_string(), "Post 7011".to_owned()));
    // It already has its video now.
    assert!(h.app.attach_original(post, &file).await.is_err());
}
