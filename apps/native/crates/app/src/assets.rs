//! The app's asset source: Lucide icons vendored from the web's `lucide-react@0.564.0` with the
//! web's 1.75 stroke baked in (design-system §8), plus gpui-component's own icon set, which its
//! `IconName` widgets load by path.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Svg, prelude::*, svg};

macro_rules! lucide {
    ($($variant:ident => $file:literal,)*) => {
        /// A vendored Lucide icon. Render with `Icon::X.svg()` and size and color it at the call site.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon { $($variant,)* }

        impl Icon {
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/lucide/", $file, ".svg"),)* }
            }
        }

        const LUCIDE: &[(&str, &[u8])] = &[
            $((concat!("icons/lucide/", $file, ".svg"), include_bytes!(concat!("../assets/icons/lucide/", $file, ".svg"))),)*
        ];
    };
}

lucide! {
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    ChevronsDownUp => "chevrons-down-up",
    ChevronsUpDown => "chevrons-up-down",
    Columns2 => "columns-2",
    Copy => "copy",
    FileDiff => "file-diff",
    FileMinus => "file-minus",
    FilePlus => "file-plus",
    FileSymlink => "file-symlink",
    FolderTree => "folder-tree",
    Maximize2 => "maximize-2",
    Minimize2 => "minimize-2",
    PanelRight => "panel-right",
    Pilcrow => "pilcrow",
    Plus => "plus",
    RefreshCw => "refresh-cw",
    Rows3 => "rows-3",
    TextWrap => "text-wrap",
    X => "x",
}

impl Icon {
    pub fn svg(self) -> Svg {
        svg().path(self.path()).flex_none()
    }
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        match LUCIDE.iter().find(|(candidate, _)| *candidate == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_component_assets::Assets.list(path)?;
        paths.extend(LUCIDE.iter().filter(|(candidate, _)| candidate.starts_with(path)).map(|(candidate, _)| SharedString::from(*candidate)));
        Ok(paths)
    }
}
