//! The layers a detection definition can come from: one directory on disk
//! and the definitions bundled into the binary.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cli::DetectionDirResolution;
use crate::config::DaemonConfig;
use crate::detector::definition::{
    Definition, DefinitionError, bundled_definition_names, bundled_definition_source,
    is_valid_detection_name, load_definition, parse_definition,
};

/// Which layers a daemon reads detection definitions from.
///
/// `directory` is the single directory layer (`None` when there is none) and
/// `bundled` says whether the definitions compiled into this binary are
/// available. A directory the user pointed at switches `bundled` off, so the
/// names it carries are the only ones that resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionLayers {
    /// The directory layer, or `None` when no directory resolved.
    pub directory: Option<PathBuf>,
    /// Whether the bundled definitions are a layer as well.
    pub bundled: bool,
}

impl DetectionLayers {
    /// The layers a startup resolution implies: an explicit directory answers
    /// alone, a config-root directory is overlaid on the bundled definitions,
    /// and with no directory the bundled definitions are the only layer left.
    ///
    /// Never panics.
    #[must_use]
    pub fn from_resolution(resolution: DetectionDirResolution) -> Self {
        match resolution {
            DetectionDirResolution::Explicit(path) => Self {
                directory: Some(path),
                bundled: false,
            },
            DetectionDirResolution::ConfigRoot(path) => Self {
                directory: Some(path),
                bundled: true,
            },
            DetectionDirResolution::Unresolved => Self {
                directory: None,
                bundled: true,
            },
        }
    }

    /// The layers a daemon carries on its configuration, i.e. what
    /// [`DetectionLayers::from_resolution`] produced at startup.
    ///
    /// Never panics.
    #[must_use]
    pub fn from_config(config: &DaemonConfig) -> Self {
        Self {
            directory: config.detection_dir.clone(),
            bundled: config.bundled_definitions,
        }
    }

    /// Whether no layer is configured at all: there is no directory layer and
    /// the bundled one is switched off, so no name can resolve and every
    /// window relying on these layers stays `Unknown`.
    ///
    /// Never panics.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.directory.is_none() && !self.bundled
    }

    /// Resolves the detection definition named `name` against these layers.
    ///
    /// The directory layer, when configured, always answers first: a name it
    /// carries -- valid or not -- is never handed to the bundled layer
    /// instead. The bundled layer only answers a name the directory layer
    /// does not have a file for at all (or when there is no directory layer).
    ///
    /// # Errors
    ///
    /// [`LayerError::Disabled`] when neither layer is configured;
    /// [`LayerError::InvalidName`] when `name` fails
    /// [`is_valid_detection_name`]; [`LayerError::Directory`] when the
    /// directory layer has a file for `name` that fails to read or parse;
    /// [`LayerError::Unscannable`] when no layer answered and the directory
    /// layer could not be scanned, so whether it carries `name` was never
    /// established; [`LayerError::NotFound`] when `name` is absent from every
    /// layer that was searched. Never panics.
    pub fn resolve(&self, name: &str) -> Result<ResolvedDefinition, LayerError> {
        if self.is_disabled() {
            return Err(LayerError::Disabled);
        }
        if !is_valid_detection_name(name) {
            return Err(LayerError::InvalidName);
        }

        if let Some(dir) = self.directory.as_ref() {
            let path = dir.join(format!("{name}.toml"));
            match load_definition(&path) {
                Ok(definition) => {
                    let (bytes, modified) = read_metadata(&path);
                    return Ok(ResolvedDefinition {
                        definition,
                        source: DefinitionSource::Directory {
                            path,
                            bytes,
                            modified,
                        },
                    });
                }
                // Only a missing file falls through: the directory layer has
                // no answer for this name at all. Any other read or parse
                // failure is that layer's own and must not be hidden.
                Err(DefinitionError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(LayerError::Directory {
                        path,
                        source: Box::new(source),
                    });
                }
            }
        }

        // Bundled sources are parsed at build time, so a parse failure here is unreachable.
        if self.bundled
            && let Some(source) = bundled_definition_source(name)
            && let Ok(definition) = parse_definition(source)
        {
            return Ok(ResolvedDefinition {
                definition,
                source: DefinitionSource::Bundled,
            });
        }

        // No layer answered. Only the inventory tells "absent from the
        // layers" apart from "never actually looked", so ask it before
        // claiming either.
        if self.list().warning.is_some() {
            return Err(LayerError::Unscannable {
                name: name.to_string(),
            });
        }

        Err(LayerError::NotFound {
            name: name.to_string(),
            layers: self.clone(),
        })
    }

    /// Lists every detection definition name available across these layers.
    ///
    /// `names` is the byte-order union of `directory_names` and
    /// `bundled_names`. A directory layer that cannot be scanned still
    /// contributes `bundled_names` (with a `warning` explaining the scan
    /// failure) rather than emptying the whole listing. Never panics.
    #[must_use]
    pub fn list(&self) -> DefinitionListing {
        let (directory_names, warning) = match self.directory.as_ref() {
            Some(dir) => scan_directory_names(dir),
            None => (Vec::new(), None),
        };
        let bundled_names: Vec<String> = if self.bundled {
            bundled_definition_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        } else {
            Vec::new()
        };

        let mut names: Vec<String> = directory_names
            .iter()
            .cloned()
            .chain(bundled_names.iter().cloned())
            .collect();
        names.sort();
        names.dedup();

        DefinitionListing {
            names,
            directory_names,
            bundled_names,
            warning,
        }
    }
}

/// A definition [`DetectionLayers::resolve`] found, together with which layer
/// it came from.
#[derive(Debug)]
pub struct ResolvedDefinition {
    /// The loaded, validated definition.
    pub definition: Definition,
    /// Which layer it was read from.
    pub source: DefinitionSource,
}

/// Which layer a [`ResolvedDefinition`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionSource {
    /// The definitions compiled into this binary.
    Bundled,
    /// A file on disk. `bytes` and `modified` come from the same
    /// [`std::fs::metadata`] call and are both `None` when it failed:
    /// a definition still loads successfully without its file's size or
    /// modification time.
    Directory {
        /// The file this definition was read from.
        path: PathBuf,
        /// The file's size in bytes, when its metadata could be read.
        bytes: Option<u64>,
        /// The file's last modification time, when its metadata could be
        /// read.
        modified: Option<SystemTime>,
    },
}

/// What [`DetectionLayers::list`] found: the union of both layers' names,
/// plus each layer's own contribution and any directory-scan warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionListing {
    /// The byte-order union of `directory_names` and `bundled_names`.
    pub names: Vec<String>,
    /// The names found in the directory layer, in byte order. Empty when
    /// there is no directory layer, or it could not be scanned.
    pub directory_names: Vec<String>,
    /// The bundled layer's names, in byte order, or empty when the bundled
    /// layer is not part of these layers.
    pub bundled_names: Vec<String>,
    /// English reason text when the directory layer could not be scanned.
    /// `None` when there is no directory layer, or it scanned successfully.
    pub warning: Option<String>,
}

/// Why [`DetectionLayers::resolve`] could not resolve a name.
#[derive(Debug)]
pub enum LayerError {
    /// Neither layer is configured.
    Disabled,
    /// The requested name fails [`is_valid_detection_name`].
    InvalidName,
    /// The directory layer has a file for the requested name that failed to
    /// read or parse.
    Directory {
        /// The path the directory layer tried to read.
        path: PathBuf,
        /// The underlying read or parse failure, boxed to keep `LayerError` small.
        source: Box<DefinitionError>,
    },
    /// No layer answered and the directory layer could not be scanned, so
    /// whether it carries the requested name was never established. The
    /// `Display` quotes the name alone: the path and the scan failure reach
    /// the daemon log from the scan itself, not a client's reply.
    Unscannable {
        /// The name that was requested.
        name: String,
    },
    /// The requested name was absent from every layer that was searched.
    NotFound {
        /// The name that was requested.
        name: String,
        /// The layers that were searched for it.
        layers: DetectionLayers,
    },
}

impl fmt::Display for LayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => write!(
                f,
                "no detection definition layer is configured; status stays unknown"
            ),
            Self::InvalidName => write!(f, "invalid detection name; status stays unknown"),
            Self::Directory { source, .. } => write!(f, "{source}"),
            Self::Unscannable { name } => write!(
                f,
                "the detection definitions could not be listed, so whether {name:?} is \
                 available could not be confirmed"
            ),
            // Each combination gets its own sentence: naming a layer that was
            // never searched would state a fact the resolver does not hold.
            Self::NotFound {
                name,
                layers:
                    DetectionLayers {
                        directory: Some(dir),
                        bundled: true,
                    },
            } => write!(
                f,
                "no detection definition named {name:?} was found in the directory {} or in \
                 the bundled layer",
                dir.display()
            ),
            Self::NotFound {
                name,
                layers:
                    DetectionLayers {
                        directory: Some(dir),
                        bundled: false,
                    },
            } => write!(
                f,
                "no detection definition named {name:?} was found in the directory {}",
                dir.display()
            ),
            Self::NotFound {
                name,
                layers:
                    DetectionLayers {
                        directory: None,
                        bundled: true,
                    },
            } => write!(
                f,
                "no detection definition named {name:?} was found in the bundled layer"
            ),
            Self::NotFound {
                name,
                layers:
                    DetectionLayers {
                        directory: None,
                        bundled: false,
                    },
            } => write!(
                f,
                "no detection definition named {name:?} was found; no layer was searched for it"
            ),
        }
    }
}

impl std::error::Error for LayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Directory { source, .. } => Some(source.as_ref()),
            Self::Disabled
            | Self::InvalidName
            | Self::Unscannable { .. }
            | Self::NotFound { .. } => None,
        }
    }
}

/// The `(bytes, modified)` pair a [`DefinitionSource::Directory`] carries,
/// both `None` when `path`'s metadata could not be read.
fn read_metadata(path: &Path) -> (Option<u64>, Option<SystemTime>) {
    match std::fs::metadata(path) {
        Ok(metadata) => (Some(metadata.len()), metadata.modified().ok()),
        Err(_) => (None, None),
    }
}

/// Scans `dir` for valid detection definition names: every `<name>.toml`
/// whose stem passes [`is_valid_detection_name`], in byte order. A directory
/// that cannot be read answers an empty list with a warning; an individual
/// unreadable entry is skipped rather than failing the whole scan.
fn scan_directory_names(dir: &Path) -> (Vec<String>, Option<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            let reason = format!("failed to enumerate detection definitions ({err})");
            tracing::warn!(dir = %dir.display(), "{reason}");
            return (Vec::new(), Some(reason));
        }
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), "skipping an unreadable directory entry ({err})");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if is_valid_detection_name(stem) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    (names, None)
}
