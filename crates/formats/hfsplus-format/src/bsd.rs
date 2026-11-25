//! BSD file kinds and owner flags stored in HFS+ catalog permissions.

/// Mask for the file-kind bits in the mode.
pub const TYPE_MASK: u16 = 0o170000;
/// Mode type for a regular file.
pub const REGULAR_FILE: u16 = 0o100000;
/// Mode type for a directory.
pub const DIRECTORY: u16 = 0o040000;
/// Mode type for a symbolic link.
pub const SYMLINK: u16 = 0o120000;
/// Owner flag set when logical file data is stored using compression.
pub const COMPRESSED: u8 = 1 << 5;
