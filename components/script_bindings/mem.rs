/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Routines for handling measuring the memory usage of arbitrary DOM nodes.

use std::os::raw::c_void;

use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};

/// Measure an Rc-backed DOM object without consuming the reflector's strong reference.
/// Safety: `obj` must be the live data pointer produced by Rc::into_raw for T.
#[expect(unsafe_code)]
pub(crate) unsafe fn malloc_size_of_including_raw_rc_self<T: MallocSizeOf>(
    ops: &mut MallocSizeOfOps,
    obj: *const c_void,
) -> usize {
    use malloc_size_of::MallocUnconditionalSizeOf;
    let object = std::mem::ManuallyDrop::new(unsafe { std::rc::Rc::from_raw(obj.cast::<T>()) });
    object.unconditional_size_of(ops)
}


/// Used by codegen to include the pointer to the `MallocSizeOf` implementation of each
/// IDL interface. This way we don't have to find the most-derived interface of DOM
/// objects by hand in code.
#[expect(unsafe_code)]
pub(crate) unsafe fn malloc_size_of_including_raw_self<T: MallocSizeOf>(
    ops: &mut MallocSizeOfOps,
    obj: *const c_void,
) -> usize {
    unsafe { ops.malloc_size_of(obj) + (*(obj as *const T)).size_of(ops) }
}
