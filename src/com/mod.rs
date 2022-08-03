// This file contains the structures references by both the gui and manager side of the
// application.
// TODO -- split this file after opengl merge

use std::fmt;
use std::ops::{Index, IndexMut};
use std::path::PathBuf;

use derive_more::{Deref, DerefMut, Display, From};
use tokio::sync::oneshot;

pub use self::displayable::*;
pub use self::res::*;


mod displayable;
mod res;


#[derive(Debug, PartialEq, Eq, Clone)]
pub enum LayoutCount {
    ExactlyOne,
    OneOrMore,
    TwoOrMore,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum OffscreenContent {
    // There is definitely nothing there, paginating is definitely pointless.
    Nothing,
    // An error, video, or other thing that is unscrollable.
    // This can include scrollable items that haven't been checked yet.
    LayoutIncompatible,
    LayoutCompatible(LayoutCount),
    // We didn't even look to see what it was, or it's past the preload limits and we're in manga
    // mode.
    Unknown,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum GuiContent {
    // TODO -- consider this or moving things to another page.
    // Single {
    //   current: Displayable,
    //   preload: Option<Displayable>,
    // }
    Single(Displayable),
    Multiple {
        prev: OffscreenContent,
        current_index: usize,
        visible: Vec<Displayable>,
        next: OffscreenContent,
    },
}

impl Default for GuiContent {
    fn default() -> Self {
        Self::Single(Displayable::default())
    }
}

#[derive(Debug, Display, Default, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    #[default]
    Single,
    VerticalStrip,
    HorizontalStrip,
    DualPage,
    DualPageReversed,
}

impl DisplayMode {
    pub const fn vertical_pagination(self) -> bool {
        match self {
            Self::Single | Self::VerticalStrip | Self::DualPage | Self::DualPageReversed => true,
            Self::HorizontalStrip => false,
        }
    }

    pub const fn half_width_pages(self) -> bool {
        match self {
            Self::DualPage | Self::DualPageReversed => true,
            Self::Single | Self::VerticalStrip | Self::HorizontalStrip => false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    pub manga: bool,
    pub upscaling: bool,
    pub fit: Fit,
    pub display: DisplayMode,
}

impl Modes {
    pub fn gui_str(self) -> String {
        let mut out = String::default();
        if self.upscaling {
            out.push('U')
        }
        if self.manga {
            out.push('M');
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Direction {
    Absolute,
    Forwards,
    Backwards,
}

pub type CommandResponder = oneshot::Sender<serde_json::Value>;

pub type MAWithResponse = (ManagerAction, GuiActionContext, Option<CommandResponder>);

#[derive(Debug, PartialEq, Eq)]
pub enum ManagerAction {
    Resolution(Res),
    MovePages(Direction, usize),
    NextArchive,
    PreviousArchive,
    Open(Vec<PathBuf>),
    // Add(Vec<PathBuf>),
    Status,
    ListPages,
    Execute(String),
    ToggleUpscaling,
    ToggleManga,
    FitStrategy(Fit),
    Display(DisplayMode),
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WorkParams {
    pub park_before_scale: bool,
    pub jump_downscaling_queue: bool,
    pub extract_early: bool,
    pub target_res: TargetRes,
}

// Represents the current displayable and its metadata.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GuiState {
    pub content: GuiContent,
    pub page_num: usize,
    pub page_name: String,
    pub archive_len: usize,
    pub archive_name: String,
    pub current_dir: PathBuf,
    pub modes: Modes,
    pub target_res: TargetRes,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Pagination {
    Forwards,
    Backwards,
}

// What to do with the scroll state after switching pages or otherwise changing the state.
#[derive(Debug, Default, Eq, PartialEq, Copy, Clone)]
pub enum ScrollMotionTarget {
    #[default]
    Maintain,
    Start,
    End,
    Continuous(Pagination),
}

impl ScrollMotionTarget {
    pub const fn continue_current_scroll(self) -> bool {
        match self {
            Self::Maintain | Self::Continuous(_) => true,
            Self::Start | Self::End => false,
        }
    }
}

// Any additional data the Gui sends along. This is not used or persisted by the manager, and is
// echoed back as context for the Gui to prevent concurrent actions from confusing the Gui.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GuiActionContext {
    pub scroll_motion_target: ScrollMotionTarget,
}

impl From<ScrollMotionTarget> for GuiActionContext {
    fn from(scroll_motion_target: ScrollMotionTarget) -> Self {
        Self { scroll_motion_target }
    }
}

#[derive(Debug)]
pub enum GuiAction {
    State(GuiState, GuiActionContext),
    Action(String, CommandResponder),
    // IdleUnload
    Quit,
}

#[derive(Deref, Default, DerefMut, From)]
pub struct DebugIgnore<T>(pub T);

impl<T> fmt::Debug for DebugIgnore<T> {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Result::Ok(())
    }
}

#[derive(Debug)]
pub struct DedupedVec<T> {
    deduped: Vec<T>,
    indices: Vec<usize>,
}

impl<T> Index<usize> for DedupedVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.deduped[self.indices[index]]
    }
}

impl<T> IndexMut<usize> for DedupedVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.deduped[self.indices[index]]
    }
}

impl<T> DedupedVec<T> {
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn iter_deduped_mut(&mut self) -> std::slice::IterMut<T> {
        self.deduped.iter_mut()
    }

    pub fn map<U, F>(&self, f: F) -> DedupedVec<U>
    where
        F: FnMut(&T) -> U,
    {
        DedupedVec {
            deduped: self.deduped.iter().map(f).collect(),
            indices: self.indices.clone(),
        }
    }
}
