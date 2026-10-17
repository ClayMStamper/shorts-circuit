//! Destination accounts. In this increment every account is manual-handoff only.

use shorts_domain::{AccountId, ConnectedAccount, Platform};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::views::AccountView;

impl App {
    /// Adds a destination that is published to through export packages.
    pub async fn add_manual_account(
        &self,
        platform: Platform,
        display_name: &str,
    ) -> AppResult<AccountView> {
        if display_name.trim().is_empty() {
            return Err(AppError::Invalid("give the account a name".into()));
        }
        let account = ConnectedAccount::manual(platform, display_name, self.now());
        self.store.accounts().insert(&account).await?;
        Ok(AccountView::from(&account))
    }

    pub async fn rename_account(
        &self,
        id: AccountId,
        display_name: &str,
    ) -> AppResult<AccountView> {
        if display_name.trim().is_empty() {
            return Err(AppError::Invalid("give the account a name".into()));
        }
        let mut account = self.store.accounts().require(id).await?;
        display_name.trim().clone_into(&mut account.display_name);
        account.updated_at = self.now();
        self.store.accounts().update(&account).await?;
        Ok(AccountView::from(&account))
    }

    pub async fn list_accounts(&self) -> AppResult<Vec<AccountView>> {
        Ok(self
            .store
            .accounts()
            .list()
            .await?
            .iter()
            .map(AccountView::from)
            .collect())
    }
}
