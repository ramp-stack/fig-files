//! Live search using the same public CHK controls as Contacts and Messages.
use chk::{
    Bumper, Context, Display, Input, Item, NavFn, Offset, Page, PageType, State, Theme, Watcher,
    design_system::{Header, PageCallbacks, PageView, TextInputEvent},
};
use prism::events::{DynEvent, ListenCallback};
use std::{cell::RefCell, rc::Rc};

pub(crate) fn page<W: Watcher>(
    label: &'static str,
    watcher: W,
    render: impl Fn(&mut Context, &Theme, W::Output, &str) -> Vec<Display> + Clone + 'static,
) -> Page
where
    W::Output: Clone + 'static,
{
    let input_id = uuid::Uuid::new_v4();
    let label_lowercase = label.to_lowercase();
    let lifecycle = Box::new(
        move |_: &mut Context, theme: &Theme, _: Option<NavFn>, _: usize| -> PageCallbacks {
            let snapshot = Rc::new(RefCell::new(None::<W::Output>));
            let current = snapshot.clone();
            let source = watcher.clone();
            let render_update = render.clone();
            let style = theme.clone();
            let on_create: ListenCallback<PageView> = Box::new(move |page, ctx| {
                page.watch(ctx, source, move |ctx, page, value| {
                    *current.borrow_mut() = Some(value.clone());
                    let query = page
                        .content()
                        .children()
                        .first()
                        .and_then(|input| Input::state(&**input));
                    let query = match &query {
                        Some(State::Text(query)) => query.as_str(),
                        _ => "",
                    };
                    let results = render_update(ctx, &style, value, query);
                    replace_results(ctx, page, &style, results);
                });
            });
            let style = theme.clone();
            let render_input = render.clone();
            let mut query = String::new();
            let on_event = Box::new(
                move |ctx: &mut Context, page: &mut PageView, event: &dyn DynEvent| {
                    if let Some(
                        TextInputEvent::Changed { id, value } | TextInputEvent::Set { id, value },
                    ) = event.downcast_ref()
                        && *id == input_id
                        && *value != query
                    {
                        query.clone_from(value);
                        if let Some(snapshot) = snapshot.borrow().as_ref().cloned() {
                            let results = render_input(ctx, &style, snapshot, &query);
                            replace_results(ctx, page, &style, results);
                            page.content_mut().children.set_scroll(0.0);
                        }
                    }
                    false
                },
            );
            let mut first_show = true;
            let on_show = Box::new(move |ctx: &mut Context| {
                if first_show {
                    first_show = false;
                    ctx.emit(TextInputEvent::Focus { id: input_id });
                }
            });
            (Some(on_event), Some(on_show), Some(on_create))
        },
    );
    Page::Static(Box::new(PageType::Content {
        header: Header::stack(&format!("Search {label_lowercase}"), None),
        items: vec![Item::Input(Input::Text {
            id: input_id,
            label: label.into(),
            actions: None,
            show_label: false,
            preset: None,
            placeholder: format!("Search {label_lowercase}..."),
        })],
        bumper: Bumper::None,
        offset: Offset::Start,
        next: None,
        flow_len: 1,
        lifecycle: Some(lifecycle),
        identity: None,
        reveal_child: None,
    }))
}

fn replace_results(ctx: &mut Context, page: &mut PageView, theme: &Theme, results: Vec<Display>) {
    // Retain the input itself, including its text, selection, and keyboard focus.
    let children = page.content_mut().children_mut();
    children.truncate(1);
    children.extend(results.into_iter().flat_map(|item| item.build(ctx, theme)));
}
