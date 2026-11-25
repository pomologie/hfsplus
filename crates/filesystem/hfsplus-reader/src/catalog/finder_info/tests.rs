use super::{EMPTY_FINDER_INFO, Profile};

#[test]
fn regular_and_directory_finder_info_preserve_visible_bytes() {
	let stored = *b"ABCDEFGHIJKLMNOPQRSTUVWXabcdef12";
	let visible = *b"ABCDEFGHIJKLMNOP\0\0\0\0\0\0\0\0abcd\0\0\0\0";
	for profile in [Profile::RegularFile, Profile::Directory] {
		assert_eq!(profile.project(stored), Some(visible));
	}
	assert_eq!(Profile::Other.project(stored), Some(stored));
}

#[test]
fn symlink_finder_info_omits_type_and_creator_and_detects_absence() {
	let stored = *b"slnkrhapIJKLMNOPQRSTUVWXabcdef12";
	let visible = *b"\0\0\0\0\0\0\0\0IJKLMNOP\0\0\0\0\0\0\0\0abcd\0\0\0\0";
	assert_eq!(Profile::Symlink.project(stored), Some(visible));

	let private_only = *b"slnkrhap\0\0\0\0\0\0\0\0QRSTUVWX\0\0\0\0yz12";
	assert_eq!(Profile::Symlink.project(private_only), None);
	assert_eq!(Profile::RegularFile.project(EMPTY_FINDER_INFO), None);
}
