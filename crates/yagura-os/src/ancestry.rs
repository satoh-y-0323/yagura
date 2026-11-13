//! The processes above one process, read from the operating system, and the
//! verdict on whether that process runs under a window's CLI alone.
//!
//! A request in a window's name is the window's own only when, up to the
//! window's PTY child, exactly one of the requester's ancestors runs an
//! image the window's profile declares: two are a CLI nested in the
//! window's CLI. A lineage that cannot be traced to the PTY child is
//! refused, never guessed. Images are opaque names to this module.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use crate::ProcessStart;

/// The most steps up from the requester (its parent is the first) at which
/// the PTY child is looked for.
const MOST_STEPS: usize = 32;

/// One process of a lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestor {
    /// Its id.
    pub pid: u32,
    /// Its executable: a path, or the file name alone as [`read_lineage`]
    /// gives it.
    pub image: String,
    /// Its start, which tells it from a later process with the same id.
    pub start: ProcessStart,
}

/// Whether two image names that differ only in letter case are the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCase {
    /// The same, as Windows file names are.
    Insensitive,
    /// Different, as Unix file names are.
    Sensitive,
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
    case: NameCase,
    end: Option<LineageEnd>,
}

impl Lineage {
    /// A lineage of already checked `ancestors`, nearest first, whose image
    /// names compare under `case`.
    #[must_use]
    pub fn new(ancestors: Vec<Ancestor>, case: NameCase) -> Self {
        Self {
            ancestors,
            case,
            end: None,
        }
    }

    fn ended(ancestors: Vec<Ancestor>, case: NameCase, end: Option<LineageEnd>) -> Self {
        Self {
            ancestors,
            case,
            end,
        }
    }

    /// The ancestors, nearest first.
    #[must_use]
    pub fn ancestors(&self) -> &[Ancestor] {
        &self.ancestors
    }
}

/// The type of an image name: the last element of an image's path, with at
/// most one `*` standing for any run of characters (none included).
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
    // Not in the design: a type no last path element can match would refuse
    // every request of its window without a word, so it is refused as read.
    /// Empty, or holding a path separator.
    #[error("the image pattern {0:?} is not a file name")]
    NotAName(String),
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
        Ok(Self { head, tail })
    }
}

impl ImagePattern {
    fn matches(&self, image: &str, case: NameCase) -> bool {
        let name = image.rsplit('/').next().unwrap_or(image);
        let name = fold(name, case);
        let head = fold(&self.head, case);
        match &self.tail {
            None => name == head,
            Some(tail) => {
                let tail = fold(tail, case);
                name.len() >= head.len() + tail.len()
                    && name.starts_with(head.as_ref())
                    && name.ends_with(tail.as_ref())
            }
        }
    }
}

/// `text` with each letter that has a one-letter upper case replaced by it,
/// as NTFS compares names, when `case` folds.
fn fold(text: &str, case: NameCase) -> Cow<'_, str> {
    match case {
        NameCase::Sensitive => Cow::Borrowed(text),
        NameCase::Insensitive => Cow::Owned(
            text.chars()
                .map(|letter| {
                    let mut upper = letter.to_uppercase();
                    match (upper.next(), upper.next()) {
                        (Some(single), None) => single,
                        _ => letter,
                    }
                })
                .collect(),
        ),
    }
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
/// start), counting the ancestors up to and including it whose image one of
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
                .any(|pattern| pattern.matches(&step.image, lineage.case))
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
/// raw `steps` from its parent up: each a `/proc/<pid>/stat` line and the
/// `/proc/<pid>/exe` link, each step the parent of the one before it. Starts
/// are read against `root`'s boot, as the guest's kernel counts them. The
/// lineage stops before a step that cannot be read (a line or link that is
/// not one, or held bytes the way here could not carry, as the replacement
/// character shows) or that is not the parent its child named.
#[must_use]
pub fn guest_lineage<'a>(
    root: &GuestRoot,
    steps: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Lineage {
    let mut ancestors: Vec<Ancestor> = Vec::new();
    let mut named_parent: Option<u32> = None;
    for (stat, exe) in steps {
        let Some((step, parent)) = guest_step(&root.boot, stat, exe) else {
            return Lineage::ended(ancestors, NameCase::Sensitive, Some(LineageEnd::Unanswered));
        };
        if named_parent.is_some_and(|named| named != step.pid) {
            return Lineage::ended(ancestors, NameCase::Sensitive, Some(LineageEnd::Reused));
        }
        ancestors.push(step);
        if parent == 0 {
            return Lineage::ended(ancestors, NameCase::Sensitive, Some(LineageEnd::Top));
        }
        named_parent = Some(parent);
    }
    Lineage::ended(ancestors, NameCase::Sensitive, None)
}

/// One step of a guest's lineage and the parent its stat line names.
fn guest_step(boot: &str, stat: &str, exe: &str) -> Option<(Ancestor, u32)> {
    if stat.contains(char::REPLACEMENT_CHARACTER) || exe.contains(char::REPLACEMENT_CHARACTER) {
        return None;
    }
    let pid = crate::process_identity::stat_pid(stat)?;
    let (_, ticks) = crate::process_identity::state_and_start(stat)?;
    let (parent, _, _) = crate::process_identity::family(stat)?;
    let image = proc_exe_image(exe)?;
    let start = format!("{boot}:{ticks}").parse().ok()?;
    Some((Ancestor { pid, image, start }, parent))
}

/// The last element of an executable path; `None` when it has none.
fn file_name(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    (!name.is_empty()).then(|| name.to_owned())
}

/// The image of a `/proc/<pid>/exe` link. proc(5): the link of an unlinked
/// executable holds " (deleted)" appended to the path it was started from,
/// as when a CLI replaces its own file to update; that mark alone is taken
/// off. Read on any host, as a WSL guest's links reach a Windows daemon too.
fn proc_exe_image(link: &str) -> Option<String> {
    file_name(link.strip_suffix(" (deleted)").unwrap_or(link))
}

#[cfg(windows)]
mod platform {
    use super::{Ancestor, Lineage, LineageEnd, MOST_STEPS, NameCase, windows_link_holds};
    use crate::platform_windows::process_snapshot;
    use crate::process_identity::process_creation;

    pub(super) fn read_lineage(pid: u32) -> Lineage {
        let (ancestors, end) = walk(pid);
        Lineage::ended(ancestors, NameCase::Insensitive, end)
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
            ancestors.push(Ancestor {
                pid: parent,
                image: parent_entry.image.clone(),
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
    use super::{Ancestor, Lineage, LineageEnd, MOST_STEPS, NameCase, unix_link_holds};
    use crate::platform_unix::executable_path;
    use crate::process_identity::process_facts;

    pub(super) fn read_lineage(pid: u32) -> Lineage {
        let (ancestors, end) = walk(pid);
        Lineage::ended(ancestors, NameCase::Sensitive, end)
    }

    #[cfg(target_os = "linux")]
    fn image_of(path: &std::path::Path) -> Option<String> {
        super::proc_exe_image(&path.to_string_lossy())
    }

    /// XNU's `proc_pidpathinfo_internal` fails rather than marks a path that
    /// no longer looks up, so the path is taken as it is.
    #[cfg(target_os = "macos")]
    fn image_of(path: &std::path::Path) -> Option<String> {
        super::file_name(&path.to_string_lossy())
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
            let Some(image) = executable_path(before.ppid).and_then(|path| image_of(&path)) else {
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
    use super::{Lineage, LineageEnd, NameCase};

    pub(super) fn read_lineage(_pid: u32) -> Lineage {
        Lineage::ended(
            Vec::new(),
            NameCase::Sensitive,
            Some(LineageEnd::Unanswered),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::proc_exe_image as executable_image;

    #[test]
    fn an_unlinked_executable_reads_as_the_name_it_was_started_from() {
        let started = executable_image("/home/u/.local/bin/agy");
        assert_eq!(started.as_deref(), Some("agy"));
        assert_eq!(
            executable_image("/home/u/.local/bin/agy (deleted)"),
            started
        );
    }

    #[test]
    fn only_the_trailing_mark_is_taken_off() {
        assert_eq!(
            executable_image("/bin/agy (deleted).old").as_deref(),
            Some("agy (deleted).old")
        );
        assert_eq!(
            executable_image("/bin/agy (deleted) (deleted)").as_deref(),
            Some("agy (deleted)")
        );
        assert_eq!(
            executable_image("/bin/agy  (deleted)").as_deref(),
            Some("agy ")
        );
        assert_eq!(
            executable_image("/opt/x (deleted)/agy").as_deref(),
            Some("agy")
        );
    }

    #[test]
    fn a_path_without_a_name_is_no_image() {
        assert_eq!(executable_image(""), None);
        assert_eq!(executable_image(" (deleted)"), None);
        assert_eq!(executable_image("/usr/bin/"), None);
        assert_eq!(executable_image("/usr/bin/ (deleted)"), None);
    }
}
