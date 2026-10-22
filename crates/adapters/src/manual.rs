//! Manual handoff: a folder with the rendered video, the copy, and posting steps, for accounts
//! (or applications) without API publishing access.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_domain::platform::full_caption;
use shorts_domain::youtube;
use shorts_domain::{
    ConnectedAccount, MediaProperties, Platform, Publication, PublicationId, ValidationIssue,
};

use crate::contract::AdapterError;

/// What goes into a package.
#[derive(Debug, Clone, Copy)]
pub struct PackageRequest<'a> {
    pub publication: &'a Publication,
    pub account: &'a ConnectedAccount,
    pub asset_title: &'a str,
    pub video_path: &'a Path,
    pub media: &'a MediaProperties,
    pub size_bytes: u64,
    /// Extra notes for the person posting, e.g. an audio-permission warning.
    pub warnings: &'a [String],
    pub exported_at: DateTime<Utc>,
}

/// Machine-readable description of a package (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub format: u32,
    pub publication_id: PublicationId,
    pub revision: u32,
    pub platform: Platform,
    pub account: String,
    pub caption: String,
    pub hashtags: Vec<String>,
    pub title: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub video_file: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub issues: Vec<ValidationIssue>,
    pub warnings: Vec<String>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportedPackage {
    pub dir: PathBuf,
    pub video: PathBuf,
    pub manifest: PackageManifest,
}

/// Writes publishing packages under `root`.
#[derive(Debug, Clone)]
pub struct ManualHandoff {
    root: PathBuf,
}

const MANIFEST_FORMAT: u32 = 1;

impl ManualHandoff {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder a package for this publication revision goes to.
    pub fn package_dir(&self, publication: &Publication, asset_title: &str) -> PathBuf {
        let date = publication
            .schedule()
            .map(|s| s.local().format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unscheduled".to_owned());
        let id = publication.id().to_string();
        self.root.join(format!(
            "{date}_{}_{}_{}_r{}",
            publication.platform().as_str(),
            slug(asset_title),
            &id[id.len() - 8..],
            publication.revision().number,
        ))
    }

    /// Writes the package. The folder appears atomically: it is assembled under a temporary
    /// name and renamed into place, replacing an earlier export of the same revision.
    pub async fn export(&self, req: &PackageRequest<'_>) -> Result<ExportedPackage, AdapterError> {
        let p = req.publication;
        let content = p.content();
        let issues = p.platform().requirements().check(
            req.media,
            req.size_bytes,
            &content.caption,
            &content.hashtags,
        );
        let title =
            (p.platform() == Platform::YoutubeShorts).then(|| youtube::title(&content.caption));
        let manifest = PackageManifest {
            format: MANIFEST_FORMAT,
            publication_id: p.id(),
            revision: p.revision().number,
            platform: p.platform(),
            account: req.account.display_name.clone(),
            caption: content.caption.clone(),
            hashtags: content.hashtags.clone(),
            title: title.clone(),
            scheduled_for: p.schedule().map(|s| s.at),
            timezone: p.schedule().map(|s| s.timezone.name().to_owned()),
            video_file: "video.mp4".to_owned(),
            duration_ms: req.media.duration_ms,
            width: req.media.width,
            height: req.media.height,
            size_bytes: req.size_bytes,
            issues: issues.clone(),
            warnings: req.warnings.to_vec(),
            exported_at: req.exported_at,
        };

        let final_dir = self.package_dir(p, req.asset_title);
        let staging = final_dir.with_extension("partial");
        let io = |e: std::io::Error| AdapterError::Transient {
            message: format!("writing the package: {e}"),
            retry_after: None,
        };
        let _ = tokio::fs::remove_dir_all(&staging).await;
        tokio::fs::create_dir_all(&staging).await.map_err(io)?;

        tokio::fs::copy(req.video_path, staging.join("video.mp4"))
            .await
            .map_err(io)?;
        let caption = full_caption(&content.caption, &content.hashtags);
        tokio::fs::write(staging.join("caption.txt"), with_newline(&caption))
            .await
            .map_err(io)?;
        let tags = content
            .hashtags
            .iter()
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(" ");
        tokio::fs::write(staging.join("hashtags.txt"), with_newline(&tags))
            .await
            .map_err(io)?;
        if let Some(title) = &title {
            tokio::fs::write(staging.join("title.txt"), with_newline(title))
                .await
                .map_err(io)?;
        }
        tokio::fs::write(
            staging.join("POSTING-STEPS.md"),
            posting_steps(req, &manifest, &caption),
        )
        .await
        .map_err(io)?;
        let json = serde_json::to_string_pretty(&manifest).expect("manifest serializes");
        tokio::fs::write(staging.join("manifest.json"), json)
            .await
            .map_err(io)?;

        if tokio::fs::metadata(&final_dir).await.is_ok() {
            tokio::fs::remove_dir_all(&final_dir).await.map_err(io)?;
        }
        tokio::fs::rename(&staging, &final_dir).await.map_err(io)?;
        Ok(ExportedPackage {
            video: final_dir.join("video.mp4"),
            dir: final_dir,
            manifest,
        })
    }
}

fn with_newline(s: &str) -> String {
    format!("{s}\n")
}

/// Lowercase ASCII letters and digits joined by `-`, at most 40 characters.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "video".to_owned()
    } else {
        out
    }
}

fn posting_steps(req: &PackageRequest<'_>, m: &PackageManifest, caption: &str) -> String {
    let mut s = String::new();
    let platform = m.platform.display_name();
    let _ = writeln!(s, "# Post to {platform}\n");
    let _ = writeln!(s, "- Account: **{}**", m.account);
    match req.publication.schedule() {
        Some(schedule) => {
            let _ = writeln!(
                s,
                "- Post at: **{}** ({}; {} UTC)",
                schedule.local().format("%A %-d %B %Y, %H:%M"),
                schedule.timezone,
                schedule.at.format("%Y-%m-%d %H:%M"),
            );
        }
        None => {
            let _ = writeln!(s, "- Post at: as soon as convenient");
        }
    }
    let _ = writeln!(
        s,
        "- Video: `video.mp4` ({}x{}, {:.1} s)",
        m.width,
        m.height,
        m.duration_ms as f64 / 1000.0
    );
    let _ = writeln!(
        s,
        "- Publication: {} (revision {})\n",
        m.publication_id, m.revision
    );

    if !m.issues.is_empty() || !m.warnings.is_empty() {
        let _ = writeln!(s, "## Check first\n");
        for issue in &m.issues {
            let _ = writeln!(s, "- {issue}");
        }
        for warning in &m.warnings {
            let _ = writeln!(s, "- {warning}");
        }
        let _ = writeln!(s);
    }

    let _ = writeln!(s, "## Steps\n");
    for (i, step) in steps_for(m.platform).iter().enumerate() {
        let _ = writeln!(s, "{}. {step}", i + 1);
    }
    let _ = writeln!(
        s,
        "{}. When it is live, open Shorts Circuit, find this publication in the monitor, choose \
         **Mark as posted** and paste the post's URL.\n",
        steps_for(m.platform).len() + 1
    );
    let _ = writeln!(s, "## Copy\n");
    if let Some(title) = &m.title {
        let _ = writeln!(s, "Title (`title.txt`):\n\n```text\n{title}\n```\n");
    }
    let _ = writeln!(s, "Caption (`caption.txt`):\n\n```text\n{caption}\n```");
    s
}

fn steps_for(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::YoutubeShorts => &[
            "Open YouTube Studio (or the YouTube app) signed in to the account above.",
            "Choose **Create → Upload videos** and select `video.mp4`.",
            "Paste `title.txt` into the title and `caption.txt` into the description.",
            "Under **Visibility**, publish now or choose **Schedule** with the time above.",
            "Shorts classification depends on the video being vertical or square and short enough; check the video appears under the channel's Shorts tab.",
        ],
        Platform::InstagramReels => &[
            "Open Instagram on a phone (or Meta Business Suite on the web) as the account above.",
            "Create a new **Reel** and select `video.mp4`.",
            "Paste `caption.txt` as the caption.",
            "Share now, or schedule it for the time above from the advanced settings.",
        ],
        Platform::SnapchatSpotlight => &[
            "Copy `video.mp4` to the phone signed in to the account above.",
            "In Snapchat, open Memories → Camera Roll and select the video.",
            "Choose **Send To → Spotlight**.",
            "Paste `caption.txt` as the description (Spotlight descriptions are short) and post.",
        ],
        Platform::Linkedin => &[
            "Open LinkedIn as the member or page admin shown above.",
            "Choose **Start a post → Video** and upload `video.mp4`.",
            "Paste `caption.txt` as the post text.",
            "Post now, or use the clock icon to schedule it for the time above.",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slug("Sourdough: Day 3 (final!)"), "sourdough-day-3-final");
        assert_eq!(slug("日本語"), "video");
        assert!(slug(&"word ".repeat(30)).len() <= 40);
    }
}
