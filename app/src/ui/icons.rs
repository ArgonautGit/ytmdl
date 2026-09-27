//! Inline stroke icons (24px grid), coloured by `currentColor`.

use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Search,
    Close,
    Download,
    DownloadCircle,
    Check,
    Retry,
    Back,
    ChevronRight,
    Settings,
    Music,
    Folder,
    Alert,
}

#[component]
pub fn Svg(icon: Icon, #[props(default = 24)] size: u32) -> Element {
    let body = match icon {
        Icon::Search => rsx! {
            circle { cx: "11", cy: "11", r: "7" }
            path { d: "m20 20-4.2-4.2" }
        },
        Icon::Close => rsx! {
            path { d: "M18 6 6 18M6 6l12 12" }
        },
        Icon::Download => rsx! {
            path { d: "M12 4v11m-5-5 5 5 5-5M5 20h14" }
        },
        Icon::DownloadCircle => rsx! {
            circle { cx: "12", cy: "12", r: "9.5" }
            path { d: "M12 7.5v8.5m-3.5-3.5 3.5 3.5 3.5-3.5" }
        },
        Icon::Check => rsx! {
            path { d: "m5 12.5 4.5 4.5L19 7.5" }
        },
        Icon::Retry => rsx! {
            path { d: "M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.5" }
            path { d: "M3.5 3.5v5h5" }
        },
        Icon::Back => rsx! {
            path { d: "M20 12H4m7 7-7-7 7-7" }
        },
        Icon::ChevronRight => rsx! {
            path { d: "m9 5 7 7-7 7" }
        },
        Icon::Settings => rsx! {
            path { d: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" }
            circle { cx: "12", cy: "12", r: "3" }
        },
        Icon::Music => rsx! {
            path { d: "M9 18V5l12-2v13" }
            circle { cx: "6", cy: "18", r: "3" }
            circle { cx: "18", cy: "16", r: "3" }
        },
        Icon::Folder => rsx! {
            path { d: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" }
        },
        Icon::Alert => rsx! {
            circle { cx: "12", cy: "12", r: "9.5" }
            path { d: "M12 7.5v5.5m0 3.5h.01" }
        },
    };
    rsx! {
        svg {
            class: "icon",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            {body}
        }
    }
}
