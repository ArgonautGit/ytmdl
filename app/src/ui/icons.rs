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
    Library,
    Play,
    Pause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    RepeatOne,
    Person,
    Disc,
    ChevronDown,
    More,
    Trash,
    Plus,
    Pencil,
    /// Add to playlist.
    ListPlus,
    /// Play next.
    ListStart,
    /// Add to queue.
    ListEnd,
    Playlist,
    /// Remove from a list.
    CircleMinus,
    Sync,
    Unlink,
    /// Sleep timer.
    Moon,
    /// Listening stats.
    Chart,
    /// Drag to reorder.
    Grip,
    Sort,
    /// A saved A-B section.
    Bookmark,
    Volume,
    Lyrics,
    /// Songs like this one (YouTube Music's radio).
    Radio,
    Home,
    /// Songs in the library twice.
    Copy,
    /// Tidying titles.
    Sparkles,
    /// In the library from another upload; picked.
    CheckCircle,
    /// Not picked.
    Circle,
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
        Icon::Library => rsx! {
            path { d: "M4 4v16M8 8v12M12 6v14m4-14 4 14" }
        },
        Icon::Play => rsx! {
            path { d: "M7 4.8v14.4a.8.8 0 0 0 1.2.7l11.5-7.2a.8.8 0 0 0 0-1.4L8.2 4.1a.8.8 0 0 0-1.2.7z", fill: "currentColor" }
        },
        Icon::Pause => rsx! {
            rect { x: "6", y: "4.5", width: "4", height: "15", rx: "1", fill: "currentColor" }
            rect { x: "14", y: "4.5", width: "4", height: "15", rx: "1", fill: "currentColor" }
        },
        Icon::Next => rsx! {
            path { d: "M5 5.5v13l10-6.5z", fill: "currentColor" }
            path { d: "M19 5v14" }
        },
        Icon::Previous => rsx! {
            path { d: "M19 5.5v13L9 12z", fill: "currentColor" }
            path { d: "M5 5v14" }
        },
        Icon::Shuffle => rsx! {
            path { d: "M2 18h1.4a4 4 0 0 0 3.3-1.7l6.1-8.6A4 4 0 0 1 16.1 6H22m-4-4 4 4-4 4" }
            path { d: "M2 6h1.9a4 4 0 0 1 3.6 2.2M22 18h-5.9a4 4 0 0 1-3.3-1.8l-.5-.8m5.7-1.4 4 4-4 4" }
        },
        Icon::Repeat => rsx! {
            path { d: "m17 2 4 4-4 4M3 11v-1a4 4 0 0 1 4-4h14M7 22l-4-4 4-4M21 13v1a4 4 0 0 1-4 4H3" }
        },
        Icon::RepeatOne => rsx! {
            path { d: "m17 2 4 4-4 4M3 11v-1a4 4 0 0 1 4-4h14M7 22l-4-4 4-4M21 13v1a4 4 0 0 1-4 4H3" }
            path { d: "M11 10h1v4" }
        },
        Icon::Person => rsx! {
            circle { cx: "12", cy: "8", r: "4" }
            path { d: "M4 21a8 8 0 0 1 16 0" }
        },
        Icon::Disc => rsx! {
            circle { cx: "12", cy: "12", r: "9.5" }
            circle { cx: "12", cy: "12", r: "2.5" }
        },
        Icon::ChevronDown => rsx! {
            path { d: "m6 9 6 6 6-6" }
        },
        Icon::More => rsx! {
            circle { cx: "12", cy: "5", r: "1" }
            circle { cx: "12", cy: "12", r: "1" }
            circle { cx: "12", cy: "19", r: "1" }
        },
        Icon::Trash => rsx! {
            path { d: "M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M10 11v6M14 11v6" }
        },
        Icon::Plus => rsx! {
            path { d: "M5 12h14M12 5v14" }
        },
        Icon::Pencil => rsx! {
            path { d: "M12 20h9M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" }
        },
        Icon::ListPlus => rsx! {
            path { d: "M11 12H3M16 6H3M16 18H3M18 9v6M21 12h-6" }
        },
        Icon::ListStart => rsx! {
            path { d: "M16 12H3M16 18H3M10 6H3M21 18V8a2 2 0 0 0-2-2h-5M16 8l-2-2 2-2" }
        },
        Icon::ListEnd => rsx! {
            path { d: "M16 12H3M16 6H3M10 18H3M21 6v10a2 2 0 0 1-2 2h-5M16 16l-2 2 2 2" }
        },
        Icon::Playlist => rsx! {
            path { d: "M21 15V6M12 12H3M16 6H3M12 18H3" }
            circle { cx: "18.5", cy: "15.5", r: "2.5" }
        },
        Icon::CircleMinus => rsx! {
            circle { cx: "12", cy: "12", r: "10" }
            path { d: "M8 12h8" }
        },
        Icon::Sync => rsx! {
            path { d: "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8M21 3v5h-5M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16M8 16H3v5" }
        },
        Icon::Unlink => rsx! {
            path { d: "m18.84 12.25 1.72-1.71a5 5 0 0 0-7.07-7.07l-1.72 1.71M5.17 11.75l-1.71 1.71a5 5 0 0 0 7.07 7.07l1.71-1.71M8 2v3M2 8h3M16 19v3M19 16h3" }
        },
        Icon::Moon => rsx! {
            path { d: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" }
        },
        Icon::Chart => rsx! {
            path { d: "M18 20V10M12 20V4M6 20v-6" }
        },
        Icon::Grip => rsx! {
            path { d: "M5 8h14M5 12h14M5 16h14" }
        },
        Icon::Sort => rsx! {
            path { d: "m3 16 4 4 4-4M7 20V4M21 8l-4-4-4 4M17 4v16" }
        },
        Icon::Bookmark => rsx! {
            path { d: "m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z" }
        },
        Icon::Volume => rsx! {
            path { d: "M11 5 6 9H2v6h4l5 4V5ZM15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14" }
        },
        Icon::Lyrics => rsx! {
            path { d: "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2zM8 8h8M8 12h5" }
        },
        Icon::Radio => rsx! {
            circle { cx: "12", cy: "12", r: "2" }
            path { d: "M16.24 7.76a6 6 0 0 1 0 8.49M7.76 16.24a6 6 0 0 1 0-8.49M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14" }
        },
        Icon::Home => rsx! {
            path { d: "M3 10.5 12 3l9 7.5M5 9v11h5v-6h4v6h5V9" }
        },
        Icon::Copy => rsx! {
            rect { x: "9", y: "9", width: "12", height: "12", rx: "2" }
            path { d: "M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" }
        },
        Icon::Sparkles => rsx! {
            path { d: "M10 3.5 11.7 8.3 16.5 10l-4.8 1.7L10 16.5l-1.7-4.8L3.5 10l4.8-1.7zM18 14l.9 2.1L21 17l-2.1.9L18 20l-.9-2.1L15 17l2.1-.9z" }
        },
        Icon::CheckCircle => rsx! {
            circle { cx: "12", cy: "12", r: "9.5" }
            path { d: "m8 12.3 2.8 2.7L16 9.5" }
        },
        Icon::Circle => rsx! {
            circle { cx: "12", cy: "12", r: "9.5" }
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
