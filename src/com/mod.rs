// This file contains the structures references by both the gui and manager side of the
// application.

use std::cell::Cell;
use std::ffi::OsString;
use std::fmt;
use std::ops::{Index, IndexMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneOrTwo<T> {
    One(T),
    Two(T, T),
}

impl<T> OneOrTwo<T> {
    pub const fn first(&self) -> &T {
        match self {
            Self::One(f) | Self::Two(f, _) => f,
        }
    }

    pub const fn second(&self) -> Option<&T> {
        match self {
            Self::One(_) => None,
            Self::Two(_f, s) => Some(s),
        }
    }

    pub const fn count(&self) -> usize {
        match self {
            Self::One(_) => 1,
            Self::Two(..) => 2,
        }
    }

    pub fn either(&self, cond: impl Fn(&T) -> bool) -> bool {
        match self {
            Self::One(f) => cond(f),
            Self::Two(f, s) => cond(f) || cond(s),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum GuiContent {
    Single {
        current: Displayable,
        preload: Option<Displayable>,
    },
    Dual {
        prev: OffscreenContent,
        visible: OneOrTwo<Displayable>,
        next: OffscreenContent,
        //   preload??: Option<OneOrTwo>,
    },
    Strip {
        prev: OffscreenContent,
        current_index: usize,
        visible: Vec<Displayable>,
        next: OffscreenContent,
    },
}

impl Default for GuiContent {
    fn default() -> Self {
        Self::Single {
            current: Displayable::default(),
            preload: None,
        }
    }
}

impl GuiContent {
    // Returns whether there is work ongoing for the currently displayed content.
    //
    // This doesn't cover all cases, such as if work is ongoing for additional pages past the
    // current and next ones in strip mode, which may be visible to the user. Scroll indicators,
    // past pages, and other types of preloading are also not covered.
    pub fn ongoing_work(&self) -> bool {
        match self {
            Self::Single { current, .. } => current.is_ongoing_work(),
            Self::Dual { visible, .. } => visible.either(Displayable::is_ongoing_work),
            Self::Strip { current_index, visible, .. } => {
                visible[*current_index].is_ongoing_work()
                    || visible.get(current_index + 1).is_some_and(Displayable::is_ongoing_work)
            }
        }
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
        self.dual_page()
    }

    pub const fn dual_page(self) -> bool {
        match self {
            Self::DualPage | Self::DualPageReversed => true,
            Self::Single | Self::VerticalStrip | Self::HorizontalStrip => false,
        }
    }

    pub const fn strip(self) -> bool {
        match self {
            Self::Single | Self::DualPage | Self::DualPageReversed => false,
            Self::VerticalStrip | Self::HorizontalStrip => true,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Toggle {
    Change,
    On,
    Off,
}

impl TryFrom<&str> for Toggle {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.eq_ignore_ascii_case("toggle") {
            Ok(Self::Change)
        } else if value.eq_ignore_ascii_case("on") {
            Ok(Self::On)
        } else if value.eq_ignore_ascii_case("off") {
            Ok(Self::Off)
        } else {
            Err(())
        }
    }
}

impl Toggle {
    // Returns true if something happened.
    #[must_use]
    pub fn apply(self, v: &mut bool) -> bool {
        match (self, *v) {
            (Self::Change, _) | (Self::On, false) | (Self::Off, true) => {
                *v = !*v;
                true
            }
            _ => false,
        }
    }

    // Returns true if something happened.
    #[must_use]
    pub fn apply_cell(self, v: &Cell<bool>) -> bool {
        let val = v.get();
        match (self, val) {
            (Self::Change, _) | (Self::On, false) | (Self::Off, true) => {
                v.set(!val);
                true
            }
            _ => false,
        }
    }

    pub fn run_if_change(self, v: bool, became_true: impl FnOnce(), became_false: impl FnOnce()) {
        match (self, v) {
            (Self::Change | Self::On, false) => became_true(),
            (Self::Change | Self::Off, true) => became_false(),
            _ => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManagerAction {
    Resolution(Res),
    MovePages(Direction, usize),
    NextArchive,
    PreviousArchive,
    Open(Vec<PathBuf>),
    Status(Vec<(String, OsString)>),
    ListPages,
    Execute(String, Vec<(String, OsString)>),
    Script(String, Vec<(String, OsString)>),
    Upscaling(Toggle),
    Manga(Toggle),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainingPath {
    AssertParent(Arc<Path>),
    TryParent(Arc<Path>),
    Current(Arc<Path>),
}

impl ContainingPath {
    pub fn get(&self) -> &Path {
        match self {
            Self::AssertParent(p) => p.parent().unwrap(),
            Self::TryParent(p) => p.parent().unwrap_or(p),
            Self::Current(p) => p,
        }
    }
}

// Represents the current displayable and its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiState {
    pub content: GuiContent,
    pub page_num: usize,
    pub page_info: Option<(Arc<str>, Arc<Path>)>,
    pub archive_len: usize,
    pub archive_name: Arc<str>,
    pub archive_id: u16,
    pub current_dir: ContainingPath,
    pub modes: Modes,
    pub target_res: TargetRes,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            content: GuiContent::default(),
            page_num: Default::default(),
            page_info: None,
            archive_len: Default::default(),
            archive_name: "".into(),
            archive_id: Default::default(),
            current_dir: ContainingPath::Current(Path::new("").into()),
            modes: Modes::default(),
            target_res: TargetRes::default(),
        }
    }
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

impl TryFrom<&str> for ScrollMotionTarget {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.eq_ignore_ascii_case("start") {
            Ok(Self::Start)
        } else if value.eq_ignore_ascii_case("end") {
            Ok(Self::End)
        } else if value.eq_ignore_ascii_case("current") {
            Ok(Self::Maintain)
        } else {
            Err(())
        }
    }
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
    Action(String, Option<CommandResponder>),
    BlockingWork,
    IdleUnload,
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
