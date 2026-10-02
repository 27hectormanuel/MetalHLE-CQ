/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Bridge from the emulated camera/mic stack (`AVCaptureDevice`,
//! `UIImagePickerController`, `AVAudioRecorder`, RemoteIO input) to *real*
//! host hardware on Android.
//!
//! All calls go through hand-rolled JNI (no `jni` crate dependency) into
//! static methods on `org.touchhle.android.HostMedia` — see that file for
//! the Android side (Camera2 still capture, AudioRecord streaming).
//!
//! On other platforms (or when the permission is denied / hardware absent)
//! every function here returns the "not available" answer, so emulated apps
//! see an honest "no camera / no mic" device instead of a fake one.

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_void};

    extern "C" {
        // Exported by libSDL2.so on Android (SDL_system.h).
        fn SDL_AndroidGetJNIEnv() -> *mut c_void;
    }

    /// JNI function-table slot indices. The table starts with 4 reserved
    /// pointers, hence every index is `4 + <ordinal in jni.h>`.
    mod slots {
        pub const FIND_CLASS: usize = 6;
        pub const EXCEPTION_OCCURRED: usize = 15;
        pub const EXCEPTION_CLEAR: usize = 17;
        pub const DELETE_LOCAL_REF: usize = 23;
        pub const GET_STATIC_METHOD_ID: usize = 113;
        pub const CALL_STATIC_OBJECT_METHOD_A: usize = 116;
        pub const CALL_STATIC_BOOLEAN_METHOD_A: usize = 119;
        pub const CALL_STATIC_VOID_METHOD_A: usize = 143;
        pub const NEW_STRING_UTF: usize = 167;
        pub const GET_STRING_UTF_CHARS: usize = 169;
        pub const RELEASE_STRING_UTF_CHARS: usize = 170;
        pub const GET_ARRAY_LENGTH: usize = 171;
        pub const NEW_BYTE_ARRAY: usize = 176;
        pub const GET_BYTE_ARRAY_ELEMENTS: usize = 184;
        pub const RELEASE_BYTE_ARRAY_ELEMENTS: usize = 192;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union JValue {
        l: *mut c_void,
        i: c_int,
        z: u8,
        _pad: u64,
    }

    struct Jni {
        env: *mut c_void,
    }

    impl Jni {
        fn attach() -> Option<Jni> {
            unsafe {
                let env = SDL_AndroidGetJNIEnv();
                if env.is_null() {
                    return None;
                }
                Some(Jni { env })
            }
        }

        /// Resolve a JNI function pointer from the function table reachable
        /// from `self.env`.
        ///
        /// This MUST go through `crate::android_jni`, which dereferences the
        /// `JNIEnv*` to reach `struct JNINativeInterface_`. Indexing `env`
        /// itself reads bytes that follow the 8-byte `JNIEnv` struct and
        /// returns them as "function pointers", so the very first call jumps
        /// to garbage — usually 0, killing the host process with
        /// `SIGSEGV at address 0x0`.
        ///
        /// [None] means the slot could not be resolved; every caller treats
        /// that as "host hardware unavailable" and returns its no-hardware
        /// answer, which is what these bridges return on other platforms too.
        fn slot<F>(&self, index: usize) -> Option<F> {
            // SAFETY: `self.env` came from SDL_AndroidGetJNIEnv() and was
            // checked for null in `attach()`; `index` is one of the `slots`
            // constants, whose numbers are taken from jni.h.
            unsafe { crate::android_jni::table_fn(self.env, index) }
        }

        fn exception_pending(&self) -> bool {
            let Some(f): Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void> =
                self.slot(slots::EXCEPTION_OCCURRED)
            else {
                return false;
            };
            !unsafe { f(self.env) }.is_null()
        }

        fn clear_exception(&self) {
            let Some(f): Option<unsafe extern "C" fn(*mut c_void)> =
                self.slot(slots::EXCEPTION_CLEAR)
            else {
                return;
            };
            unsafe { f(self.env) }
        }

        fn find_host_media_class(&self) -> Option<*mut c_void> {
            let name = CString::new("org/touchhle/android/HostMedia").ok()?;
            let Some(f): Option<
                unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
            > = self.slot(slots::FIND_CLASS)
            else {
                return None;
            };
            let class = unsafe { f(self.env, name.as_ptr()) };
            if class.is_null() {
                self.clear_exception();
                return None;
            }
            Some(class)
        }

        fn get_static_method(
            &self,
            class: *mut c_void,
            name: &str,
            sig: &str,
        ) -> Option<*mut c_void> {
            let name = CString::new(name).ok()?;
            let sig = CString::new(sig).ok()?;
            let Some(f): Option<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *const c_char,
                    *const c_char,
                ) -> *mut c_void,
            > = self.slot(slots::GET_STATIC_METHOD_ID)
            else {
                return None;
            };
            let method = unsafe { f(self.env, class, name.as_ptr(), sig.as_ptr()) };
            if method.is_null() {
                self.clear_exception();
                return None;
            }
            Some(method)
        }

        fn call_static_bool(
            &self,
            class: *mut c_void,
            method: *mut c_void,
            args: &[JValue],
        ) -> bool {
            let Some(f): Option<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *const JValue,
                ) -> u8,
            > = self.slot(slots::CALL_STATIC_BOOLEAN_METHOD_A)
            else {
                return false;
            };
            let r = unsafe { f(self.env, class, method, args.as_ptr()) };
            if self.exception_pending() {
                self.clear_exception();
            }
            r != 0
        }

        fn call_static_void(&self, class: *mut c_void, method: *mut c_void, args: &[JValue]) {
            let Some(f): Option<
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const JValue),
            > = self.slot(slots::CALL_STATIC_VOID_METHOD_A)
            else {
                return;
            };
            unsafe { f(self.env, class, method, args.as_ptr()) };
            if self.exception_pending() {
                self.clear_exception();
            }
        }

        fn call_static_object(
            &self,
            class: *mut c_void,
            method: *mut c_void,
            args: &[JValue],
        ) -> *mut c_void {
            let Some(f): Option<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *const JValue,
                ) -> *mut c_void,
            > = self.slot(slots::CALL_STATIC_OBJECT_METHOD_A)
            else {
                return std::ptr::null_mut();
            };
            let r = unsafe { f(self.env, class, method, args.as_ptr()) };
            if self.exception_pending() {
                self.clear_exception();
            }
            r
        }

        fn delete_local_ref(&self, obj: *mut c_void) {
            if obj.is_null() {
                return;
            }
            let Some(f): Option<unsafe extern "C" fn(*mut c_void, *mut c_void)> =
                self.slot(slots::DELETE_LOCAL_REF)
            else {
                return;
            };
            unsafe { f(self.env, obj) }
        }

        fn with_class<T>(&self, f: impl FnOnce(&Jni, *mut c_void) -> Option<T>) -> Option<T> {
            let class = self.find_host_media_class()?;
            let result = f(self, class);
            self.delete_local_ref(class);
            result
        }
    }

    fn bool_arg(v: bool) -> JValue {
        JValue {
            z: if v { 1 } else { 0 },
        }
    }

    pub fn has_camera(front: bool) -> bool {
        let Some(jni) = Jni::attach() else {
            return false;
        };
        jni.with_class(|jni, class| {
            let method = jni.get_static_method(class, "hasCamera", "(Z)Z")?;
            Some(jni.call_static_bool(class, method, &[bool_arg(front)]))
        })
        .unwrap_or(false)
    }

    /// Take a still photo; returns JPEG bytes or None.
    pub fn take_photo(front: bool) -> Option<Vec<u8>> {
        let jni = Jni::attach()?;
        jni.with_class(|jni, class| {
            let method = jni.get_static_method(class, "takePhoto", "(Z)[B")?;
            // Resolve every function pointer up front, so a slot that cannot
            // be resolved bails out before any JNI local reference exists.
            let len_f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int =
                jni.slot(slots::GET_ARRAY_LENGTH)?;
            // GetByteArrayElements(env, array, &isCopy) — the third argument
            // is an *out* parameter for "did JNI copy the bytes?", not a
            // destination buffer.
            let get_f: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut u8,
            ) -> *mut u8 = jni.slot(slots::GET_BYTE_ARRAY_ELEMENTS)?;
            let rel_f: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut u8,
                c_int,
            ) = jni.slot(slots::RELEASE_BYTE_ARRAY_ELEMENTS)?;
            let arr = jni.call_static_object(class, method, &[bool_arg(front)]);
            if arr.is_null() {
                return None;
            }
            let len = unsafe { len_f(jni.env, arr) };
            if len <= 0 {
                jni.delete_local_ref(arr);
                return None;
            }
            let mut bytes = vec![0u8; len as usize];
            let mut is_copy: u8 = 0;
            let ptr = unsafe { get_f(jni.env, arr, &mut is_copy) };
            if ptr.is_null() {
                jni.delete_local_ref(arr);
                return None;
            }
            if ptr != bytes.as_mut_ptr() {
                unsafe {
                    std::ptr::copy_nonoverlapping(ptr, bytes.as_mut_ptr(), len as usize);
                }
            }
            unsafe { rel_f(jni.env, arr, ptr, 0) };
            jni.delete_local_ref(arr);
            Some(bytes)
        })
    }

    pub fn has_microphone() -> bool {
        let Some(jni) = Jni::attach() else {
            return false;
        };
        jni.with_class(|jni, class| {
            let method = jni.get_static_method(class, "hasMicrophone", "()Z")?;
            Some(jni.call_static_bool(class, method, &[]))
        })
        .unwrap_or(false)
    }

    pub fn start_mic() -> bool {
        let Some(jni) = Jni::attach() else {
            return false;
        };
        jni.with_class(|jni, class| {
            let method = jni.get_static_method(class, "startMic", "()Z")?;
            Some(jni.call_static_bool(class, method, &[]))
        })
        .unwrap_or(false)
    }

    pub fn stop_mic() {
        if let Some(jni) = Jni::attach() {
            if let Some(class) = jni.find_host_media_class() {
                if let Some(method) = jni.get_static_method(class, "stopMic", "()V") {
                    jni.call_static_void(class, method, &[]);
                }
                jni.delete_local_ref(class);
            }
        }
    }

    /// The most recent mic chunk: mono 16-bit LE PCM samples.
    pub fn read_mic_chunk() -> Vec<i16> {
        let Some(jni) = Jni::attach() else {
            return Vec::new();
        };
        let Some(bytes) = jni.with_class(|jni, class| {
            let method = jni.get_static_method(class, "readMicChunk", "()[S")?;
            Some(jni.call_static_object(class, method, &[]))
        }) else {
            return Vec::new();
        };
        if bytes.is_null() {
            return Vec::new();
        }
        let Some(len_f): Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int> =
            jni.slot(slots::GET_ARRAY_LENGTH)
        else {
            jni.delete_local_ref(bytes);
            return Vec::new();
        };
        // GetShortArrayRegion(env, array, start, len, buf) copies a short[]
        // region straight into our buffer; index 202 in
        // `struct JNINativeInterface_` (see jni.h).
        const GET_SHORT_ARRAY_REGION: usize = 202;
        let Some(region_f): Option<
            unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_int, *mut i16),
        > = jni.slot(GET_SHORT_ARRAY_REGION)
        else {
            jni.delete_local_ref(bytes);
            return Vec::new();
        };
        let len = unsafe { len_f(jni.env, bytes) };
        let mut out = Vec::new();
        if len > 0 {
            let mut samples = vec![0i16; len as usize];
            unsafe { region_f(jni.env, bytes, 0, len, samples.as_mut_ptr()) };
            out = samples;
        }
        jni.delete_local_ref(bytes);
        out
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    pub fn has_camera(_front: bool) -> bool {
        false
    }
    pub fn take_photo(_front: bool) -> Option<Vec<u8>> {
        None
    }
    pub fn has_microphone() -> bool {
        false
    }
    pub fn start_mic() -> bool {
        false
    }
    pub fn stop_mic() {}
    pub fn read_mic_chunk() -> Vec<i16> {
        Vec::new()
    }
}

pub use imp::*;

/// Sample rate of chunks returned by [read_mic_chunk] (matches
/// `HostMedia.MIC_SAMPLE_RATE` on Android; unused elsewhere).
pub const MIC_SAMPLE_RATE: u32 = 44100;
