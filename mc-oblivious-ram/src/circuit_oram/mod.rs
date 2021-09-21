// //! Implements CircuitORAM on top of a generic ORAMStorage and a generic PositionMap.
// //!
// //! In this implementation, the bucket size (Z in paper) is configurable.
// //!
// //! The storage will hold blocks of size ValueSize * Z for the data, and
// //! MetaSize * Z for the metadata.
// //!
// //! Most papers suggest Z = 2 or Z = 4, Z = 1 probably won't work.
// //!
// //! It is expected that you want the block size to be 4096 (one linux page)
// //!
// //! Height of storage tree is set as log size - log bucket_size
// //! This is informed by Gentry et al.

// use alloc::vec;

// use aligned_cmov::{
//     subtle::{Choice, ConstantTimeEq, ConstantTimeLess},
//     typenum::{PartialDiv, Prod, Unsigned, U16, U64, U8},
//     A64Bytes, A8Bytes, ArrayLength, AsAlignedChunks, AsNeSlice, CMov,
// };
// use alloc::{boxed::Box, vec::Vec};
// use balanced_tree_index::TreeIndex;
// use core::{marker::PhantomData, ops::Mul};
// use mc_oblivious_traits::{
//     log2_ceil, ORAMStorage, ORAMStorageCreator, PositionMap, PositionMapCreator, ORAM,
// };
// use rand_core::{CryptoRng, RngCore};

// /// In this implementation, a value is expected to be an aligned 4096 byte page.
// /// The metadata associated to a value is two u64's (block num and leaf), so 16 bytes.
// /// It is stored separately from the value so as not to break alignment.
// /// In many cases block-num and leaf can be u32's. But I suspect that there will
// /// be other stuff in this metadata as well in the end so the savings isn't much.
// type MetaSize = U16;

// // A metadata object is always associated to any Value in the PathORAM structure.
// // A metadata consists of two fields: leaf_num and block_num
// // A metadata has the status of being "vacant" or "not vacant".
// //
// // The block_num is the number in range 0..len that corresponds to the user's query.
// // every block of data in the ORAM has an associated block number.
// // There should be only one non-vacant data with a given block number at a time,
// // if none is found then it will be initialized lazily on first query.
// //
// // The leaf_num is the "target" of this data in the tree, according to Path ORAM algorithm.
// // It represents a TreeIndex value. In particular it is not zero.
// //
// // The leaf_num attached to a block_num should match pos[block_num], it is a cache of that value,
// // which enables us to perform efficient eviction and packing in a branch.
// //
// // A metadata is defined to be "vacant" if leaf_num IS zero.
// // This indicates that the metadata and its corresponding value can be overwritten
// // with a real item.

// /// Get the leaf num of a metadata
// fn meta_leaf_num(src: &A8Bytes<MetaSize>) -> &u64 {
//     &src.as_ne_u64_slice()[0]
// }
// /// Get the leaf num of a mutable metadata
// fn meta_leaf_num_mut(src: &mut A8Bytes<MetaSize>) -> &mut u64 {
//     &mut src.as_mut_ne_u64_slice()[0]
// }
// /// Get the block num of a metadata
// fn meta_block_num(src: &A8Bytes<MetaSize>) -> &u64 {
//     &src.as_ne_u64_slice()[1]
// }
// /// Get the block num of a mutable metadata
// fn meta_block_num_mut(src: &mut A8Bytes<MetaSize>) -> &mut u64 {
//     &mut src.as_mut_ne_u64_slice()[1]
// }
// /// Test if a metadata is "vacant"
// fn meta_is_vacant(src: &A8Bytes<MetaSize>) -> Choice {
//     meta_leaf_num(src).ct_eq(&0)
// }
// /// Set a metadata to vacant, obliviously, if a condition is true
// fn meta_set_vacant(condition: Choice, src: &mut A8Bytes<MetaSize>) {
//     meta_leaf_num_mut(src).cmov(condition, &0);
// }

// /// An implementation of PathORAM, using u64 to represent leaves in metadata.
// pub struct CircuitORAM<ValueSize, Z, StorageType, RngType>
// where
//     ValueSize: ArrayLength<u8> + PartialDiv<U8> + PartialDiv<U64>,
//     Z: Unsigned + Mul<ValueSize> + Mul<MetaSize>,
//     RngType: RngCore + CryptoRng + Send + Sync + 'static,
//     StorageType: ORAMStorage<Prod<Z, ValueSize>, Prod<Z, MetaSize>> + Send + Sync + 'static,
//     Prod<Z, ValueSize>: ArrayLength<u8> + PartialDiv<U8>,
//     Prod<Z, MetaSize>: ArrayLength<u8> + PartialDiv<U8>,
// {
//     /// The height of the binary tree used for storage
//     height: u32,
//     /// The storage itself
//     storage: StorageType,
//     /// The position map
//     pos: Box<dyn PositionMap + Send + Sync + 'static>,
//     /// The rng
//     rng: RngType,
//     /// The stashed values
//     stash_data: Vec<A64Bytes<ValueSize>>,
//     /// The stashed metadata
//     stash_meta: Vec<A8Bytes<MetaSize>>,
//     /// Our currently checked-out branch if any
//     branch: BranchCheckout<ValueSize, Z>,
// }

// impl<ValueSize, Z, StorageType, RngType> CircuitORAM<ValueSize, Z, StorageType, RngType>
// where
//     ValueSize: ArrayLength<u8> + PartialDiv<U8> + PartialDiv<U64>,
//     Z: Unsigned + Mul<ValueSize> + Mul<MetaSize>,
//     RngType: RngCore + CryptoRng + Send + Sync + 'static,
//     StorageType: ORAMStorage<Prod<Z, ValueSize>, Prod<Z, MetaSize>> + Send + Sync + 'static,
//     Prod<Z, ValueSize>: ArrayLength<u8> + PartialDiv<U8>,
//     Prod<Z, MetaSize>: ArrayLength<u8> + PartialDiv<U8>,
// {
//     /// New function creates this ORAM given a position map creator and a
//     /// storage type creator and an Rng creator.
//     /// The main thing that is going on here is, given the size, we are determining
//     /// what the height will be, which will be like log(size) - log(bucket_size)
//     /// Then we are making sure that all the various creators use this number.
//     pub fn new<
//         PMC: PositionMapCreator<RngType>,
//         SC: ORAMStorageCreator<Prod<Z, ValueSize>, Prod<Z, MetaSize>, Output = StorageType>,
//         F: FnMut() -> RngType + 'static,
//     >(
//         size: u64,
//         stash_size: usize,
//         rng_maker: &mut F,
//     ) -> Self {
//         assert!(size != 0, "size cannot be zero");
//         assert!(size & (size - 1) == 0, "size must be a power of two");
//         // saturating_sub is used so that creating an ORAM of size 1 or 2 doesn't fail
//         let height = log2_ceil(size).saturating_sub(log2_ceil(Z::U64));
//         // This is 2u64 << height because it must be 2^{h+1}, we have defined
//         // the height of the root to be 0, so in a tree where the lowest level
//         // is h, there are 2^{h+1} nodes.
//         let mut rng = rng_maker();
//         let storage = SC::create(2u64 << height, &mut rng).expect("Storage failed");
//         let pos = PMC::create(size, height, stash_size, rng_maker);
//         Self {
//             height,
//             storage,
//             pos,
//             rng,
//             stash_data: vec![Default::default(); stash_size],
//             stash_meta: vec![Default::default(); stash_size],
//             branch: Default::default(),
//         }
//     }
// }

// impl<ValueSize, Z, StorageType, RngType> ORAM<ValueSize>
//     for CircuitORAM<ValueSize, Z, StorageType, RngType>
// where
//     ValueSize: ArrayLength<u8> + PartialDiv<U8> + PartialDiv<U64>,
//     Z: Unsigned + Mul<ValueSize> + Mul<MetaSize>,
//     RngType: RngCore + CryptoRng + Send + Sync + 'static,
//     StorageType: ORAMStorage<Prod<Z, ValueSize>, Prod<Z, MetaSize>> + Send + Sync + 'static,
//     Prod<Z, ValueSize>: ArrayLength<u8> + PartialDiv<U8>,
//     Prod<Z, MetaSize>: ArrayLength<u8> + PartialDiv<U8>,
// {
//     fn len(&self) -> u64 {
//         self.pos.len()
//     }
//     // TODO: We should try implementing a circuit-ORAM like approach also
//     fn access<T, F: FnOnce(&mut A64Bytes<ValueSize>) -> T>(&mut self, key: u64, f: F) -> T {
//         let result: T;
//         // Choose what will be the next (secret) position of this item
//         let new_pos = 1u64.random_child_at_height(self.height, &mut self.rng);
//         // Set the new value and recover the old (current) position.
//         let current_pos = self.pos.write(&key, &new_pos);
//         debug_assert!(current_pos != 0, "position map told us the item is at 0");
//         // Get the branch where we expect to find the item.
//         // NOTE: If we move to a scheme where the tree can be resized dynamically,
//         // then we should checkout at `current_pos.random_child_at_height(self.height)`.
//         debug_assert!(self.branch.leaf == 0);
//         self.branch.checkout(&mut self.storage, current_pos);

//         // Fetch the item from branch and then from stash.
//         // Visit it and then insert it into the stash.
//         {
//             debug_assert!(self.branch.leaf == current_pos);
//             let mut meta = A8Bytes::<MetaSize>::default();
//             let mut data = A64Bytes::<ValueSize>::default();

//             self.branch
//                 .ct_find_and_remove(1.into(), &key, &mut data, &mut meta);
//             details::ct_find_and_remove(
//                 1.into(),
//                 &key,
//                 &mut data,
//                 &mut meta,
//                 &mut self.stash_data,
//                 &mut self.stash_meta,
//             );
//             debug_assert!(
//                 meta_block_num(&meta) == &key || meta_is_vacant(&meta).into(),
//                 "Hmm, we didn't find the expected item something else"
//             );
//             debug_assert!(self.branch.leaf == current_pos);

//             // Call the callback, then store the result
//             result = f(&mut data);

//             // Set the block_num in case the item was not initialized yet
//             *meta_block_num_mut(&mut meta) = key;
//             // Set the new leaf destination for the item
//             *meta_leaf_num_mut(&mut meta) = new_pos;

//             // Stash the item
//             details::ct_insert(
//                 1.into(),
//                 &data,
//                 &mut meta,
//                 &mut self.stash_data,
//                 &mut self.stash_meta,
//             );
//             assert!(bool::from(meta_is_vacant(&meta)), "Stash overflow!");
//         }

//         // Now do cleanup / eviction on this branch, before checking out
//         {
//             debug_assert!(self.branch.leaf == current_pos);
//             self.branch.pack();
//             for idx in 0..self.stash_data.len() {
//                 self.branch
//                     .ct_insert(1.into(), &self.stash_data[idx], &mut self.stash_meta[idx]);
//             }
//         }

//         debug_assert!(self.branch.leaf == current_pos);
//         self.branch.checkin(&mut self.storage);
//         debug_assert!(self.branch.leaf == 0);

//         result
//     }
// }
