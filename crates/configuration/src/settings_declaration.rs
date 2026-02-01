pub trait SettingsDeclaration: Send + Sync {}
pub type SettingsDeclarationDynRef = &'static dyn SettingsDeclaration;