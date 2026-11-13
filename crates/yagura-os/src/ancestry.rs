//! The processes above one process, read from the operating system, and the
//! verdict on whether that process runs under a window's CLI alone.
//!
//! A request in a window's name is the window's own only when, up to the
//! window's PTY child, exactly one of the requester's ancestors runs under a
//! name the window's profile declares: two are a CLI nested in the window's
//! CLI. A lineage that cannot be traced to the PTY child is refused, never
//! guessed. Names are opaque to this module.

use std::fmt;
use std::str::FromStr;

use crate::ProcessStart;

/// The most steps up from the requester (its parent is the first) at which
/// the PTY child is looked for.
const MOST_STEPS: usize = 32;

/// The bytes of a name a Unix source keeps: Linux's command name and
/// macOS's `pbi_comm` both hold 16 bytes with the closing NUL.
const UNIX_NAME_BYTES: usize = 15;

/// One process of a lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestor {
    /// Its id.
    pub pid: u32,
    /// The name the operating system keeps of it from its start, which an
    /// update of its executable file does not change; compared under the
    /// lineage's [`NameRule`].
    pub image: Vec<u8>,
    /// Its start, which tells it from a later process with the same id.
    pub start: ProcessStart,
}

/// How the names of a lineage compare with the declared types, after the
/// source the names were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRule {
    /// A Windows image file name: kept whole, and compared folding letter
    /// case as NTFS does. A name that is not UTF-8 matches no type.
    Windows,
    /// A Unix command name: bytes, letter case kept. The kernel keeps only
    /// the first 15 bytes, so a name of that length may be the start of a
    /// longer one and is compared with the start of the type.
    Unix,
}

/// Why a lineage read from the operating system stops where it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageEnd {
    /// The last process has no parent.
    Top,
    /// The id a process names as its parent now names another process.
    Reused,
    /// The operating system did not answer for the next step.
    Unanswered,
}

/// The ancestors of one process, nearest first, each the real parent of the
/// one before it; the process itself is not among them.
#[derive(Debug, Clone)]
pub struct Lineage {
    ancestors: Vec<Ancestor>,
    rule: NameRule,
    end: Option<LineageEnd>,
}

impl Lineage {
    /// A lineage of already checked `ancestors`, nearest first, whose names
    /// compare under `rule`.
    #[must_use]
    pub fn new(ancestors: Vec<Ancestor>, rule: NameRule) -> Self {
        Self {
            ancestors,
            rule,
            end: None,
        }
    }

    fn ended(ancestors: Vec<Ancestor>, rule: NameRule, end: Option<LineageEnd>) -> Self {
        Self {
            ancestors,
            rule,
            end,
        }
    }

    /// The ancestors, nearest first.
    #[must_use]
    pub fn ancestors(&self) -> &[Ancestor] {
        &self.ancestors
    }
}

/// The type of a process's name, with at most one `*` standing for any run
/// of characters (none included) and something before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePattern {
    head: String,
    tail: Option<String>,
}

/// An image name type that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImagePatternError {
    /// More than one `*`.
    #[error("the image pattern {0:?} has more than one `*`")]
    ManyStars(String),
    // Not in the design: a type no name can match would refuse every request
    // of its window without a word, so it is refused as read.
    /// Empty, or holding a path separator.
    #[error("the image pattern {0:?} is not a file name")]
    NotAName(String),
    /// Starting with `*`: every 15-byte Unix name starts with an empty head,
    /// so any such process in a window's tree would count.
    #[error("the image pattern {0:?} starts with `*`")]
    EmptyHead(String),
}

impl FromStr for ImagePattern {
    type Err = ImagePatternError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.is_empty() || text.contains('/') {
            return Err(ImagePatternError::NotAName(text.to_owned()));
        }
        let mut parts = text.split('*');
        let head = parts.next().unwrap_or_default().to_owned();
        let tail = parts.next().map(str::to_owned);
        if parts.next().is_some() {
            return Err(ImagePatternError::ManyStars(text.to_owned()));
        }
        if head.is_empty() {
            return Err(ImagePatternError::EmptyHead(text.to_owned()));
        }
        Ok(Self { head, tail })
    }
}

impl ImagePattern {
    fn matches(&self, name: &[u8], rule: NameRule) -> bool {
        match rule {
            NameRule::Windows => {
                let Ok(name) = std::str::from_utf8(name) else {
                    return false;
                };
                let tail = self.tail.as_deref().map(fold);
                whole_match(
                    fold(name).as_bytes(),
                    fold(&self.head).as_bytes(),
                    tail.as_ref().map(String::as_bytes),
                )
            }
            NameRule::Unix => {
                let head = self.head.as_bytes();
                let tail = self.tail.as_ref().map(String::as_bytes);
                if name.len() == UNIX_NAME_BYTES {
                    cut_match(name, head, tail)
                } else {
                    whole_match(name, head, tail)
                }
            }
        }
    }
}

fn whole_match(name: &[u8], head: &[u8], tail: Option<&[u8]>) -> bool {
    match tail {
        None => name == head,
        Some(tail) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
    }
}

/// `name` may be the first [`UNIX_NAME_BYTES`] of a longer name. A head that
/// long decides alone; a shorter one is all there is to compare when a `*`
/// follows it, since what the `*` and the tail stand for was cut off.
fn cut_match(name: &[u8], head: &[u8], tail: Option<&[u8]>) -> bool {
    match head.get(..UNIX_NAME_BYTES) {
        Some(kept) => kept == name,
        None => tail.is_some() && name.starts_with(head),
    }
}

/// `text` with each letter that has a one-letter upper case replaced by it,
/// as NTFS compares names.
fn fold(text: &str) -> String {
    text.chars()
        .map(|letter| {
            let mut upper = letter.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(single), None) => single,
                _ => letter,
            }
        })
        .collect()
}

/// Why a lineage could not be judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Undecided {
    /// The PTY child was reached with no declared image on the way.
    NoImage,
    /// The PTY child is not within the most steps looked at.
    TooDeep,
    /// The lineage ends before the PTY child; why, when it was read from the
    /// operating system.
    Unreached(Option<LineageEnd>),
}

impl fmt::Display for Undecided {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoImage => f.write_str("no declared image up to the PTY child"),
            Self::TooDeep => write!(
                f,
                "the PTY child is not within {MOST_STEPS} steps of the requester"
            ),
            Self::Unreached(None) => f.write_str("the lineage does not reach the PTY child"),
            Self::Unreached(Some(LineageEnd::Top)) => {
                f.write_str("the lineage reaches its top before the PTY child")
            }
            Self::Unreached(Some(LineageEnd::Reused)) => {
                f.write_str("a parent's id names another process before the PTY child")
            }
            Self::Unreached(Some(LineageEnd::Unanswered)) => {
                f.write_str("the operating system did not answer before the PTY child")
            }
        }
    }
}

/// The verdict on a lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageVerdict {
    /// Exactly one declared image up to the PTY child: the window's CLI.
    Accepted,
    /// Two or more: a CLI nested in the window's CLI.
    Nested,
    /// Refused, as no verdict could be reached.
    Undecidable(Undecided),
    /// No anchor to judge against, so the lineage is not looked at.
    Unchecked,
}

/// Judges `lineage` against `anchor`, the window's PTY child (`pid` and
/// start), counting the ancestors up to and including it whose name one of
/// `images` matches; an ancestor matching several counts once.
#[must_use]
pub fn judge_lineage(
    lineage: &Lineage,
    anchor: Option<(u32, &ProcessStart)>,
    images: &[ImagePattern],
) -> LineageVerdict {
    let Some((pid, start)) = anchor else {
        return LineageVerdict::Unchecked;
    };
    let looked_at = &lineage.ancestors[..lineage.ancestors.len().min(MOST_STEPS)];
    let Some(at) = looked_at
        .iter()
        .position(|step| step.pid == pid && step.start == *start)
    else {
        let why = if lineage.ancestors.len() > MOST_STEPS {
            Undecided::TooDeep
        } else {
            Undecided::Unreached(lineage.end)
        };
        return LineageVerdict::Undecidable(why);
    };
    let declared = looked_at[..=at]
        .iter()
        .filter(|step| {
            images
                .iter()
                .any(|pattern| pattern.matches(&step.image, lineage.rule))
        })
        .count();
    match declared {
        0 => LineageVerdict::Undecidable(Undecided::NoImage),
        1 => LineageVerdict::Accepted,
        _ => LineageVerdict::Nested,
    }
}

/// Windows: whether a process created at `parent_created` can be the parent
/// of one created at `child_created` (raw creation ticks). A snapshot keeps
/// a parent's id after the parent exits, so a "parent" created after the
/// child is another process that took the id; an equal time is let through.
#[must_use]
pub fn windows_link_holds(parent_created: u64, child_created: u64) -> bool {
    parent_created <= child_created
}

/// Unix: whether the parent read between two readings of the child, each
/// its `(ppid, start)`, is the child's real parent. The kernel hands a child
/// whose parent exits to another, so an unchanged ppid and start mean the
/// parent did not exit between them. Starts are compared for equality only.
#[must_use]
pub fn unix_link_holds(before: (u32, &ProcessStart), after: (u32, &ProcessStart)) -> bool {
    before.0 == after.0 && before.1 == after.1
}

/// Reads the ancestors of the running process `pid`, nearest first, each
/// checked to be the real parent of the one before it. The lineage stops at
/// the first step that cannot be read or checked, a step past the most
/// [`judge_lineage`] looks at, or the top. Blocks on the operating system.
#[must_use]
pub fn read_lineage(pid: u32) -> Lineage {
    platform::read_lineage(pid)
}

/// The root of a WSL guest's lineage: the guest process that started the
/// window's CLI there, as the launch put it in the CLI's environment,
/// `<pid>:<boot_id>:<starttime>` (the starttime being the 22nd field of its
/// `/proc/<pid>/stat` line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRoot {
    pid: u32,
    boot: String,
    start: ProcessStart,
}

/// A guest root that is not `<pid>:<boot_id>:<starttime>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the guest root {0:?} is not <pid>:<boot_id>:<starttime>")]
pub struct GuestRootError(String);

impl FromStr for GuestRoot {
    type Err = GuestRootError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let malformed = || GuestRootError(text.to_owned());
        let (pid, rest) = text.split_once(':').ok_or_else(malformed)?;
        let (boot, ticks) = rest.rsplit_once(':').ok_or_else(malformed)?;
        let pid = pid.parse().map_err(|_| malformed())?;
        if boot.is_empty() || ticks.is_empty() || !ticks.bytes().all(|b| b.is_ascii_digit()) {
            return Err(malformed());
        }
        // The same form a Linux host gives its own starts (`process_start`).
        let start = format!("{boot}:{ticks}").parse().map_err(|_| malformed())?;
        Ok(Self {
            pid,
            boot: boot.to_owned(),
            start,
        })
    }
}

impl GuestRoot {
    /// The root as the anchor [`judge_lineage`] takes.
    #[must_use]
    pub fn anchor(&self) -> (u32, &ProcessStart) {
        (self.pid, &self.start)
    }
}

/// The lineage a WSL guest's hook command or bridge wrapper collected, as
/// raw `steps` from its parent up: each a `/proc/<pid>/stat` line, each step
/// the parent of the one before it. A step's name is the command name in its
/// line, compared under [`NameRule::Unix`]; starts are read against `root`'s
/// boot, as the guest's kernel counts them. The lineage stops before a step
/// that cannot be read (a line that is not one, or that held bytes the way
/// here could not carry, as the replacement character shows) or that is not
/// the parent its child named.
#[must_use]
pub fn guest_lineage<'a>(root: &GuestRoot, steps: impl IntoIterator<Item = &'a str>) -> Lineage {
    let mut ancestors: Vec<Ancestor> = Vec::new();
    let mut named_parent: Option<u32> = None;
    for stat in steps {
        let Some((step, parent)) = guest_step(&root.boot, stat) else {
            return Lineage::ended(ancestors, NameRule::Unix, Some(LineageEnd::Unanswered));
        };
        if named_parent.is_some_and(|named| named != step.pid) {
            return Lineage::ended(ancestors, NameRule::Unix, Some(LineageEnd::Reused));
        }
        ancestors.push(step);
        if parent == 0 {
            return Lineage::ended(ancestors, NameRule::Unix, Some(LineageEnd::Top));
        }
        named_parent = Some(parent);
    }
    Lineage::ended(ancestors, NameRule::Unix, None)
}

/// One step of a guest's lineage and the parent its stat line names.
fn guest_step(boot: &str, stat: &str) -> Option<(Ancestor, u32)> {
    if stat.contains(char::REPLACEMENT_CHARACTER) {
        return None;
    }
    let pid = crate::process_identity::stat_pid(stat)?;
    let (_, ticks) = crate::process_identity::state_and_start(stat)?;
    let (parent, _, _) = crate::process_identity::family(stat)?;
    let image = crate::process_identity::stat_name(stat)?;
    if image.is_empty() {
        return None;
    }
    let start = format!("{boot}:{ticks}").parse().ok()?;
    Some((
        Ancestor {
            pid,
            image: image.as_bytes().to_vec(),
            start,
        },
        parent,
    ))
}

#[cfg(windows)]
mod platform {
    use super::{Ancestor, Lineage, LineageEnd, MOST_STEPS, NameRule, windows_link_holds};
    use crate::platform_windows::process_snapshot;
    use crate::process_identity::process_creation;

    pub(super) fn read_lineage(pid: u32) -> Lineage {
        let (ancestors, end) = walk(pid);
        Lineage::ended(ancestors, NameRule::Windows, end)
    }

    /// Walks one snapshot from `pid` up, each parent opened for its creation
    /// time after the snapshot: an id that names another process by then was
    /// created after its child and is not followed.
    fn walk(pid: u32) -> (Vec<Ancestor>, Option<LineageEnd>) {
        let mut ancestors = Vec::new();
        let Ok(processes) = process_snapshot() else {
            return (ancestors, Some(LineageEnd::Unanswered));
        };
        let Ok(Some((mut child_created, _))) = process_creation(pid) else {
            return (ancestors, Some(LineageEnd::Unanswered));
        };
        let mut child = pid;
        while ancestors.len() <= MOST_STEPS {
            let Some(entry) = processes.get(&child) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            let parent = entry.parent;
            if parent == 0 || parent == child {
                return (ancestors, Some(LineageEnd::Top));
            }
            // A parent that has exited out of the snapshot is not judged.
            let Some(parent_entry) = processes.get(&parent) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            let Ok(Some((parent_created, start))) = process_creation(parent) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            if !windows_link_holds(parent_created, child_created) {
                return (ancestors, Some(LineageEnd::Reused));
            }
            // A name that is not UTF-16 is kept as none: no type, which
            // always has a head, matches it, and the walk goes on.
            ancestors.push(Ancestor {
                pid: parent,
                image: parent_entry
                    .image
                    .clone()
                    .map(String::into_bytes)
                    .unwrap_or_default(),
                start,
            });
            child = parent;
            child_created = parent_created;
        }
        // Past the most steps judged: the verdict is the same however it ends.
        (ancestors, None)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::{Ancestor, Lineage, LineageEnd, MOST_STEPS, NameRule, unix_link_holds};
    use crate::process_identity::process_facts;

    pub(super) fn read_lineage(pid: u32) -> Lineage {
        let (ancestors, end) = walk(pid);
        Lineage::ended(ancestors, NameRule::Unix, end)
    }

    /// Reads the child, then its parent, then the child again: the parent
    /// read is the real one only when the child reads the same both times.
    fn walk(pid: u32) -> (Vec<Ancestor>, Option<LineageEnd>) {
        let mut ancestors = Vec::new();
        let mut child = pid;
        while ancestors.len() <= MOST_STEPS {
            let Ok(Some(before)) = process_facts(child) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            if before.ppid == 0 {
                return (ancestors, Some(LineageEnd::Top));
            }
            let Ok(Some(parent)) = process_facts(before.ppid) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            let Some(image) = parent.name.filter(|name| !name.is_empty()) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            let Ok(Some(after)) = process_facts(child) else {
                return (ancestors, Some(LineageEnd::Unanswered));
            };
            if !unix_link_holds((before.ppid, &before.start), (after.ppid, &after.start)) {
                return (ancestors, Some(LineageEnd::Reused));
            }
            ancestors.push(Ancestor {
                pid: before.ppid,
                image,
                start: parent.start,
            });
            child = before.ppid;
        }
        // Past the most steps judged: the verdict is the same however it ends.
        (ancestors, None)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod platform {
    use super::{Lineage, LineageEnd, NameRule};

    pub(super) fn read_lineage(_pid: u32) -> Lineage {
        Lineage::ended(Vec::new(), NameRule::Unix, Some(LineageEnd::Unanswered))
    }
}
