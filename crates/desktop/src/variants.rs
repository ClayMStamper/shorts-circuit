//! Variant editing and rendering.

use serde_json::json;
use shorts_domain::{
    AssetId, Job, JobKind, JobState, PublicationEvent, RenderRecipe, RevisionContent,
    TransitionError, Variant, VariantId,
};
use shorts_store::StoreError;
use tracing::warn;

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::{RenderedView, VariantView};

const RENDER_MAX_FAILURES: u32 = 3;

impl App {
    pub async fn create_variant(
        &self,
        asset_id: AssetId,
        name: &str,
        recipe: RenderRecipe,
    ) -> AppResult<VariantView> {
        let media = self
            .store
            .media_files()
            .for_asset(asset_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Invalid("import media for this asset first".into()))?;
        let variant = Variant::new(
            asset_id,
            media.id,
            name,
            recipe,
            &media.properties,
            self.now(),
        )?;
        self.store.variants().insert(&variant).await?;
        self.emit(AppEvent::LibraryChanged);
        self.variant_view(&variant).await
    }

    /// Changes a variant's name and recipe. A recipe change bumps the variant revision, and
    /// every publication using the variant gets a new revision too, which drops its approval.
    pub async fn update_variant(
        &self,
        id: VariantId,
        name: &str,
        recipe: RenderRecipe,
    ) -> AppResult<VariantView> {
        let mut variant = self.store.variants().require(id).await?;
        let loaded = variant.revision();
        let media = self
            .store
            .media_files()
            .require(variant.media_file_id)
            .await?;
        let now = self.now();
        variant.rename(name, now)?;
        let changed = variant.update_recipe(recipe, &media.properties, now)?;
        self.store.variants().update(&variant, loaded).await?;
        if changed {
            self.cancel_open_render(id).await?;
            self.revise_publications_using(&variant).await?;
        }
        self.emit(AppEvent::LibraryChanged);
        self.variant_view(&variant).await
    }

    async fn revise_publications_using(&self, variant: &Variant) -> AppResult<()> {
        for publication in self.store.publications().using_variant(variant.id).await? {
            let id = publication.id();
            let result = self
                .transition(id, |p| {
                    let content = RevisionContent {
                        variant_revision: variant.revision(),
                        ..p.content().clone()
                    };
                    PublicationEvent::Revise { content }
                })
                .await;
            match result {
                Ok(_) | Err(AppError::Transition(TransitionError::Unchanged)) => {}
                // Active or finished publications keep the media they were approved with.
                Err(AppError::Transition(TransitionError::NotAllowed { .. })) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn cancel_open_render(&self, id: VariantId) -> AppResult<()> {
        if let Some(job) = self
            .store
            .jobs()
            .open_for(JobKind::RenderVariant, &id.to_string())
            .await?
        {
            let now = self.now();
            self.store
                .jobs()
                .modify(job.id(), |j| j.cancel(now))
                .await?;
        }
        Ok(())
    }

    /// Queues a render of the variant's current recipe (no-op if one is already queued).
    pub async fn render_variant(&self, id: VariantId) -> AppResult<()> {
        let variant = self.store.variants().require(id).await?;
        if variant
            .render()
            .is_some_and(|r| std::path::Path::new(&r.path).is_file())
        {
            return Ok(());
        }
        let now = self.now();
        let job = Job::new(
            JobKind::RenderVariant,
            id.to_string(),
            json!({ "revision": variant.revision() }),
            now,
            false,
            RENDER_MAX_FAILURES,
            now,
        );
        match self.store.jobs().insert(&job).await {
            Ok(()) | Err(StoreError::DuplicateOpenJob { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        self.poke();
        Ok(())
    }

    pub async fn variant(&self, id: VariantId) -> AppResult<VariantView> {
        let variant = self.store.variants().require(id).await?;
        self.variant_view(&variant).await
    }

    pub(crate) async fn variant_view(&self, v: &Variant) -> AppResult<VariantView> {
        let media = self.store.media_files().get(v.media_file_id).await?;
        let rendering = match self
            .store
            .jobs()
            .open_for(JobKind::RenderVariant, &v.id.to_string())
            .await
        {
            Ok(job) => job.is_some_and(|j| j.state() != JobState::Paused),
            Err(e) => {
                warn!(error = %e, "could not read render jobs");
                false
            }
        };
        Ok(VariantView {
            id: v.id.to_string(),
            asset_id: v.asset_id.to_string(),
            name: v.name.clone(),
            revision: v.revision(),
            recipe: v.recipe().clone(),
            rendered: v
                .render()
                .filter(|r| std::path::Path::new(&r.path).is_file())
                .map(|r| RenderedView {
                    path: r.path.clone(),
                    size_bytes: r.size_bytes,
                    properties: r.properties.clone(),
                    rendered_at: r.rendered_at,
                }),
            rendering,
            output_duration_ms: media
                .map(|m| v.recipe().output_duration_ms(m.properties.duration_ms))
                .unwrap_or_default(),
        })
    }
}
