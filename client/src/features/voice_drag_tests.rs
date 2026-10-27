use super::*;
use dioxus::core::{ElementId, Mutation};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
};

#[test]
fn leaving_a_voice_drop_target_cancels_it_and_drop_click_does_not_join_the_moderator() {
    type Export = Rc<RefCell<Option<VoiceDragContext>>>;
    let export: Export = Rc::new(RefCell::new(None));
    let joins = Rc::new(Cell::new(0));
    let channel: Channel = serde_json::from_value(serde_json::json!({
        "id": Id::new_v4(), "guild_id": Id::new_v4(), "name":"Destination", "kind":"voice", "topic":null
    })).unwrap();
    let destination = channel.id;
    let mut dom = VirtualDom::new_with_props(
        |(export, joins, channel): (Export, Rc<Cell<usize>>, Channel)| {
            let state = use_signal(AppState::empty);
            provide_context(state);
            let drag = use_context_provider(|| VoiceDragContext {
                drag: Signal::new(Some(VoiceDrag {
                    pubkey: "target".into(),
                    from_channel: Id::nil(),
                    destination: None,
                })),
                suppress_click: Signal::new(false),
            });
            *export.borrow_mut() = Some(drag);
            rsx! { VoiceChannelRow {
                channel, connected: false, occupants: vec![], self_pubkey: None,
                on_join: move |_| joins.set(joins.get() + 1),
            } }
        },
        (export.clone(), joins.clone(), channel),
    );
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let initial = dom.rebuild_to_vec().edits;
    let listener = |name: &str| {
        initial
            .iter()
            .find_map(|edit| match edit {
                Mutation::NewEventListener { name: event, id } if event == name => Some(*id),
                _ => None,
            })
            .unwrap()
    };
    fn pointer(dom: &mut VirtualDom, name: &str, id: ElementId) {
        let mut value = serde_json::to_value(dioxus::html::SerializedMouseData::default()).unwrap();
        value.as_object_mut().unwrap().extend(
            serde_json::json!({
                "pointer_id":1,"width":1.0,"height":1.0,"pressure":0.0,"tangential_pressure":0.0,
                "tilt_x":0,"tilt_y":0,"twist":0,"pointer_type":"mouse","is_primary":true
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let data: dioxus::html::SerializedPointerData = serde_json::from_value(value).unwrap();
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(data)));
        dom.runtime()
            .handle_event(name, Event::new(data, false), id);
        dom.render_immediate_to_vec();
    }
    pointer(&mut dom, "pointerenter", listener("pointerenter"));
    let mut drag = export.borrow().unwrap();
    assert_eq!(
        drag.drag.read().as_ref().unwrap().destination,
        Some(destination)
    );
    pointer(&mut dom, "pointerleave", listener("pointerleave"));
    assert_eq!(drag.drag.read().as_ref().unwrap().destination, None);
    let click = listener("click");
    for suppressed in [true, false] {
        drag.suppress_click.set(suppressed);
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        dom.runtime()
            .handle_event("click", Event::new(data, true), click);
        dom.render_immediate_to_vec();
        assert_eq!(joins.get(), usize::from(!suppressed));
    }
}
