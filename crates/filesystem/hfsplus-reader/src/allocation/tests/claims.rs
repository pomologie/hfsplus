use core::convert::Infallible;

use super::{AllocationError, Claim, Claims, MetadataOwner};
use crate::{SpecialFile, alloc::vec::Vec};

#[test]
fn extent_bounds_preserve_claims_and_allow_the_exact_end() {
	let total = 8;
	let last_block = total - 1;
	let primary = MetadataOwner::PrimaryRegion;
	let alternate = MetadataOwner::AlternateRegion;
	let catalog = MetadataOwner::SpecialFile(SpecialFile::Catalog);
	let extents = MetadataOwner::SpecialFile(SpecialFile::Extents);
	let mut claims = Claims {
		ranges: Vec::from([Claim {
			blocks: 0..1,
			owner: primary,
		}]),
		total,
		limit: 2,
	};

	for (start, length, owner) in [
		(last_block, 0, alternate),
		(u32::MAX, 1, catalog),
		(last_block, 2, extents),
	] {
		assert!(matches!(
			claims.push::<Infallible>(start, length, owner),
			Err(AllocationError::ExtentBounds { owner: reported }) if reported == owner
		));
		assert_eq!(claims.ranges.len(), 1);
		assert_eq!(claims.ranges[0].blocks, 0..1);
		assert_eq!(claims.ranges[0].owner, primary);
	}

	claims.push::<Infallible>(last_block, 1, alternate).unwrap();
	assert_eq!(claims.ranges.len(), 2);
	assert_eq!(claims.ranges[1].blocks, last_block..total);
	assert_eq!(claims.ranges[1].owner, alternate);
}
