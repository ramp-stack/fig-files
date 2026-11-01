use chk::{App, Assets, ChkTheme, Color, Context, Icons, RootInfo, Theme};
use prism::drawable::Drawable;

mod browser;
pub mod collaboration;
mod collaboration_ui;
pub mod contacts;
mod contacts_ui;
pub mod drafts;
mod pdf_preview;
mod runtime;
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
        "Fig Files".into()
    }

    fn new(ctx: &mut Context) -> impl Drawable {
        let assets = Assets::new(vec![include_dir::include_dir!(
            "$CARGO_MANIFEST_DIR/resources"
        )]);
        let theme = FigFiles.theme().to_pelican(assets.clone());
        runtime::FileRuntime::new(Box::new(FigFiles.build(ctx, assets)), theme)
    }
}

impl App for FigFiles {
    fn roots(&self, ctx: &mut Context, theme: &Theme) -> Vec<RootInfo> {
        let vault = storage::open(&ctx.air);
        sharing::open(&ctx.air);
        collaboration::open(&ctx.air);
        drafts::open(&ctx.air);
        contacts::open(&ctx.air);
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
        ]
    }

    fn theme(&self) -> ChkTheme {
        ChkTheme::Dark(Color::from_hex("#D7FF62", 255))
    }
}
