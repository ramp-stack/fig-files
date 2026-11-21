use chk::{App, Assets, ChkTheme, Color, Context, Icons, RootInfo, Theme};
use prism::drawable::Drawable;

mod browser;
pub mod collaboration;
mod collaboration_ui;
pub mod contacts;
mod contacts_ui;
pub mod drafts;
mod invitation_delivery;
mod invitations;
mod pdf_preview;
mod preview;
mod runtime;
mod search;
pub mod sharing;
mod sharing_ui;
mod svg_preview;

pub mod storage;
#[cfg(test)]
mod ui_tests;

ramp::run!(FigFiles);

pub struct FigFiles;

impl prism::Application for FigFiles {
    fn name() -> String {
        <Self as App>::name()
    }

    fn new(ctx: &mut Context) -> impl Drawable {
        let app = <Self as App>::new(ctx);
        let theme = app.theme().build::<chk::PelicanUi>(app.assets());
        runtime::FileRuntime::new(app.build(ctx), theme)
    }
}

impl App for FigFiles {
    type DesignSystem = chk::PelicanUi;
    type Invitation = invitations::Invitation;

    fn name() -> String {
        "Fig Files".into()
    }

    fn new(ctx: &mut Context) -> Self {
        invitation_delivery::start(&ctx.air);
        Self
    }

    fn invitations(&self, _: &mut Context) -> Option<chk::Invitations<Self::Invitation>> {
        Some(invitations::router())
    }

    fn assets(&self) -> Assets {
        Assets::new(vec![include_dir::include_dir!(
            "$CARGO_MANIFEST_DIR/resources"
        )])
    }

    fn roots(
        &self,
        ctx: &mut Context,
        theme: &Theme,
        _: Option<&chk::Invitations<Self::Invitation>>,
    ) -> Vec<RootInfo> {
        let vault = storage::open(&ctx.air);
        sharing::open(&ctx.air);
        collaboration::open(&ctx.air);
        drafts::open(&ctx.air);
        contacts::open(&ctx.air);
        contacts_ui::sync_profiles(ctx);
        let profile = contacts_ui::profile_root(ctx);
        vec![
            RootInfo::icon(
                ctx,
                theme,
                Icons::Home,
                "Files",
                chk::Root::custom(browser::listing(
                    vault.clone(),
                    browser::Location::Folder(None),
                    true,
                )),
            ),
            RootInfo::icon(
                ctx,
                theme,
                Icons::Send,
                "Shared",
                chk::Root::custom(sharing_ui::home()),
            ),
            RootInfo::icon(
                ctx,
                theme,
                Icons::Group,
                "Contacts",
                chk::Root::custom(contacts_ui::home()),
            ),
            RootInfo::icon(
                ctx,
                theme,
                Icons::Delete,
                "Trash",
                chk::Root::custom(browser::listing(vault, browser::Location::Trash, true)),
            ),
            RootInfo::icon(ctx, theme, Icons::Profile, "Profile", profile),
        ]
    }

    fn theme(&self) -> ChkTheme {
        ChkTheme::Dark(Color::from_hex("#D7FF62", 255))
    }
}
