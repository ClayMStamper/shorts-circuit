//! Variants: an edit recipe applied to an original, plus its (regenerable) rendered output.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{AssetId, MediaFileId, VariantId};
use crate::media::MediaProperties;
use crate::recipe::{RecipeIssue, RenderRecipe};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VariantError {
    #[error("recipe is invalid: {0:?}")]
    InvalidRecipe(Vec<RecipeIssue>),
    #[error("render is for recipe revision {render} but the variant is at revision {current}")]
    StaleRender { render: u32, current: u32 },
    #[error("name must not be empty")]
    EmptyName,
}

/// A finished render. Lives in the render cache; can be regenerated from the recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedOutput {
    pub path: String,
    pub cache_key: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
    pub rendered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub id: VariantId,
    pub asset_id: AssetId,
    pub media_file_id: MediaFileId,
    pub name: String,
    recipe: RenderRecipe,
    /// Incremented on every recipe change. Publication revisions record the value they used.
    revision: u32,
    render: Option<RenderedOutput>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Variant {
    pub fn new(
        asset_id: AssetId,
        media_file_id: MediaFileId,
        name: &str,
        recipe: RenderRecipe,
        source: &MediaProperties,
        now: DateTime<Utc>,
    ) -> Result<Self, VariantError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(VariantError::EmptyName);
        }
        let issues = recipe.validate(Some(source));
        if !issues.is_empty() {
            return Err(VariantError::InvalidRecipe(issues));
        }
        Ok(Self {
            id: VariantId::new(),
            asset_id,
            media_file_id,
            name: name.to_owned(),
            recipe,
            revision: 1,
            render: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rebuilds a variant from storage.
    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        id: VariantId,
        asset_id: AssetId,
        media_file_id: MediaFileId,
        name: String,
        recipe: RenderRecipe,
        revision: u32,
        render: Option<RenderedOutput>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            asset_id,
            media_file_id,
            name,
            recipe,
            revision,
            render,
            created_at,
            updated_at,
        }
    }

    pub fn recipe(&self) -> &RenderRecipe {
        &self.recipe
    }

    pub fn revision(&self) -> u32 {
        self.revision
    }

    pub fn render(&self) -> Option<&RenderedOutput> {
        self.render.as_ref()
    }

    /// Replaces the recipe. Returns `Ok(true)` when it changed, which bumps the revision and
    /// discards the old render; publications that used the old revision must be revised.
    pub fn update_recipe(
        &mut self,
        recipe: RenderRecipe,
        source: &MediaProperties,
        now: DateTime<Utc>,
    ) -> Result<bool, VariantError> {
        let issues = recipe.validate(Some(source));
        if !issues.is_empty() {
            return Err(VariantError::InvalidRecipe(issues));
        }
        if recipe == self.recipe {
            return Ok(false);
        }
        self.recipe = recipe;
        self.revision += 1;
        self.render = None;
        self.updated_at = now;
        Ok(true)
    }

    pub fn rename(&mut self, name: &str, now: DateTime<Utc>) -> Result<(), VariantError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(VariantError::EmptyName);
        }
        self.name = name.to_owned();
        self.updated_at = now;
        Ok(())
    }

    /// Records a finished render made from `for_revision`. Renders of an older revision are
    /// rejected so a slow job can't overwrite a newer edit.
    pub fn attach_render(
        &mut self,
        for_revision: u32,
        output: RenderedOutput,
        now: DateTime<Utc>,
    ) -> Result<(), VariantError> {
        if for_revision != self.revision {
            return Err(VariantError::StaleRender {
                render: for_revision,
                current: self.revision,
            });
        }
        self.render = Some(output);
        self.updated_at = now;
        Ok(())
    }

    /// The render was evicted from the cache.
    pub fn clear_render(&mut self, now: DateTime<Utc>) {
        self.render = None;
        self.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{AudioChoice, TrimRange};

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn source() -> MediaProperties {
        MediaProperties {
            duration_ms: 30_000,
            width: 1080,
            height: 1920,
            audio_streams: 1,
            ..Default::default()
        }
    }

    fn output() -> RenderedOutput {
        RenderedOutput {
            path: "cache/x.mp4".into(),
            cache_key: "k".into(),
            size_bytes: 1,
            properties: source(),
            rendered_at: now(),
        }
    }

    fn variant() -> Variant {
        Variant::new(
            AssetId::new(),
            MediaFileId::new(),
            "YouTube cut",
            RenderRecipe::default(),
            &source(),
            now(),
        )
        .unwrap()
    }

    #[test]
    fn recipe_changes_bump_revision_and_drop_render() {
        let mut v = variant();
        v.attach_render(1, output(), now()).unwrap();
        let changed = v
            .update_recipe(
                RenderRecipe {
                    audio: AudioChoice::Mute,
                    ..Default::default()
                },
                &source(),
                now(),
            )
            .unwrap();
        assert!(changed);
        assert_eq!(v.revision(), 2);
        assert!(v.render().is_none());
    }

    #[test]
    fn identical_recipe_is_a_no_op() {
        let mut v = variant();
        v.attach_render(1, output(), now()).unwrap();
        assert!(
            !v.update_recipe(RenderRecipe::default(), &source(), now())
                .unwrap()
        );
        assert_eq!(v.revision(), 1);
        assert!(v.render().is_some());
    }

    #[test]
    fn invalid_recipes_are_rejected() {
        let mut v = variant();
        let bad = RenderRecipe {
            trim: Some(TrimRange {
                start_ms: 0,
                end_ms: 60_000,
            }),
            ..Default::default()
        };
        assert!(matches!(
            v.update_recipe(bad, &source(), now()),
            Err(VariantError::InvalidRecipe(_))
        ));
        assert_eq!(v.revision(), 1);
    }

    #[test]
    fn stale_renders_are_rejected() {
        let mut v = variant();
        v.update_recipe(
            RenderRecipe {
                audio: AudioChoice::Mute,
                ..Default::default()
            },
            &source(),
            now(),
        )
        .unwrap();
        assert_eq!(
            v.attach_render(1, output(), now()),
            Err(VariantError::StaleRender {
                render: 1,
                current: 2
            })
        );
    }
}
