#[macro_export]
macro_rules! host_tests {
    ( $TestRegion:path ) => {
        use lazy_static::lazy_static;
        use libc::c_void;
        use lucet_runtime::vmctx::{lucet_vmctx, Vmctx};
        use lucet_runtime::{
            lucet_hostcall_terminate, lucet_hostcalls, DlModule, Error, Limits, Region,
            TerminationDetails, TrapCode,
        };
        use std::sync::{Arc, Mutex};
        use $TestRegion as TestRegion;
        use $crate::build::test_module_c;
        use $crate::helpers::{FunctionPointer, MockExportBuilder, MockModuleBuilder};
        #[test]
        fn load_module() {
            let _module = test_module_c("host", "trivial.c").expect("build and load module");
        }

        #[test]
        fn load_nonexistent_module() {
            let module = DlModule::load("/non/existient/file");
            assert!(module.is_err());
        }

        const ERROR_MESSAGE: &'static str = "hostcall_test_func_hostcall_error";

        lazy_static! {
            static ref HOSTCALL_MUTEX: Mutex<()> = Mutex::new(());
            static ref NESTED_OUTER: Mutex<()> = Mutex::new(());
            static ref NESTED_INNER: Mutex<()> = Mutex::new(());
            static ref NESTED_REGS_OUTER: Mutex<()> = Mutex::new(());
            static ref NESTED_REGS_INNER: Mutex<()> = Mutex::new(());
        }

        #[inline]
        unsafe fn unwind_outer(vmctx: &mut Vmctx, mutex: &Mutex<()>, cb_idx: u32) -> u64 {
            let lock = mutex.lock().unwrap();
            let func = vmctx
                .get_func_from_idx(0, cb_idx)
                .expect("can get function by index");
            let func = std::mem::transmute::<usize, extern "C" fn(*mut lucet_vmctx) -> u64>(
                func.ptr.as_usize(),
            );
            let res = (func)(vmctx.as_raw());
            drop(lock);
            res
        }

        #[allow(unreachable_code)]
        #[inline]
        unsafe fn unwind_inner(vmctx: &mut Vmctx, mutex: &Mutex<()>) {
            let lock = mutex.lock().unwrap();
            lucet_hostcall_terminate!(ERROR_MESSAGE);
            drop(lock);
        }

        lucet_hostcalls! {
            #[no_mangle]
            pub unsafe extern "C" fn hostcall_test_func_hello(
                &mut vmctx,
                hello_ptr: u32,
                hello_len: u32,
            ) -> () {
                let heap = vmctx.heap();
                let hello = heap.as_ptr() as usize + hello_ptr as usize;
                if !vmctx.check_heap(hello as *const c_void, hello_len as usize) {
                    lucet_hostcall_terminate!("heap access");
                }
                let hello = std::slice::from_raw_parts(hello as *const u8, hello_len as usize);
                if hello.starts_with(b"hello") {
                    *vmctx.get_embed_ctx_mut::<bool>() = true;
                }
            }

            #[no_mangle]
            pub unsafe extern "C" fn hostcall_test_func_hostcall_error(
                &mut _vmctx,
            ) -> () {
                lucet_hostcall_terminate!(ERROR_MESSAGE);
            }

            #[allow(unreachable_code)]
            #[no_mangle]
            pub unsafe extern "C" fn hostcall_test_func_hostcall_error_unwind(
                &mut vmctx,
            ) -> () {
                let lock = HOSTCALL_MUTEX.lock().unwrap();
                unsafe {
                    lucet_hostcall_terminate!(ERROR_MESSAGE);
                }
                drop(lock);
            }

            #[no_mangle]
            pub unsafe extern "C" fn nested_error_unwind_outer(
                &mut vmctx,
                cb_idx: u32,
            ) -> u64 {
                unwind_outer(vmctx, &*NESTED_OUTER, cb_idx)
            }

            #[no_mangle]
            pub unsafe extern "C" fn nested_error_unwind_inner(
                &mut vmctx,
            ) -> () {
                unwind_inner(vmctx, &*NESTED_INNER)
            }

            #[no_mangle]
            pub unsafe extern "C" fn nested_error_unwind_regs_outer(
                &mut vmctx,
                cb_idx: u32,
            ) -> u64 {
                unwind_outer(vmctx, &*NESTED_REGS_OUTER, cb_idx)
            }

            #[no_mangle]
            pub unsafe extern "C" fn nested_error_unwind_regs_inner(
                &mut vmctx,
            ) -> () {
                unwind_inner(vmctx, &*NESTED_REGS_INNER)
            }

            #[no_mangle]
            pub unsafe extern "C" fn hostcall_bad_borrow(
                &mut vmctx,
            ) -> bool {
                let heap = vmctx.heap();
                let mut other_heap = vmctx.heap_mut();
                heap[0] == other_heap[0]
            }

            #[no_mangle]
            pub unsafe extern "C" fn hostcall_missing_embed_ctx(
                &mut vmctx,
            ) -> bool {
                struct S {
                    x: bool
                }
                let ctx = vmctx.get_embed_ctx::<S>();
                ctx.x
            }

            #[no_mangle]
            pub unsafe extern "C" fn hostcall_multiple_vmctx(
                &mut vmctx,
            ) -> bool {
                let mut vmctx1 = Vmctx::from_raw(vmctx.as_raw());
                vmctx1.heap_mut()[0] = 0xAF;
                drop(vmctx1);

                let mut vmctx2 = Vmctx::from_raw(vmctx.as_raw());
                let res = vmctx2.heap()[0] == 0xAF;
                drop(vmctx2);

                res
            }

            #[no_mangle]
            pub unsafe extern "C" fn hostcall_print_host_rsp(
                &mut vmctx,
            ) -> () {
                use lucet_runtime_internals::instance::HOST_CTX;
                use lucet_runtime_internals::vmctx::VmctxInternal;
                let inst = vmctx.instance();
                eprintln!("guest's context at {:p}", &inst.ctx);
                inst.alloc.slot.as_ref().map(|slot| {
                    eprintln!("guest's stack highest addr = {:p}", slot.stack_top());
                });
                HOST_CTX.with(|host_ctx| {
                    let ctx = host_ctx.get();
                    eprintln!("host's context at {:p}", ctx);
                    eprintln!("host's stored rsp = 0x{:x}", (*ctx).gpr.rsp);
                });
            }
        }

        #[test]
        fn instantiate_trivial() {
            let module = test_module_c("host", "trivial.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let inst = region
                .new_instance(module)
                .expect("instance can be created");
        }

        #[test]
        fn run_trivial() {
            let module = test_module_c("host", "trivial.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");
            inst.run("main", &[0u32.into(), 0i32.into()])
                .expect("instance runs");
        }

        #[test]
        fn run_hello() {
            let module = test_module_c("host", "hello.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");

            let mut inst = region
                .new_instance_builder(module)
                .with_embed_ctx(false)
                .build()
                .expect("instance can be created");

            inst.run("main", &[0u32.into(), 0i32.into()])
                .expect("instance runs");

            assert!(*inst.get_embed_ctx::<bool>().unwrap().unwrap());
        }

        #[test]
        fn run_hostcall_error() {
            let module = test_module_c("host", "hostcall_error.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("main", &[0u32.into(), 0i32.into()]) {
                Err(Error::RuntimeTerminated(term)) => {
                    assert_eq!(
                        *term
                            .provided_details()
                            .expect("user provided termination reason")
                            .downcast_ref::<&'static str>()
                            .expect("error was static str"),
                        ERROR_MESSAGE
                    );
                }
                res => panic!("unexpected result: {:?}", res),
            }
        }

        #[test]
        fn run_hostcall_error_unwind() {
            let module =
                test_module_c("host", "hostcall_error_unwind.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("main", &[0u32.into(), 0u32.into()]) {
                Err(Error::RuntimeTerminated(term)) => {
                    assert_eq!(
                        *term
                            .provided_details()
                            .expect("user provided termination reason")
                            .downcast_ref::<&'static str>()
                            .expect("error was static str"),
                        ERROR_MESSAGE
                    );
                }
                res => panic!("unexpected result: {:?}", res),
            }

            assert!(HOSTCALL_MUTEX.is_poisoned());
        }

        /// Check that if two segments of hostcall stack are present when terminating, that they
        /// both get properly unwound.
        #[test]
        fn nested_error_unwind() {
            let module =
                test_module_c("host", "nested_error_unwind.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("entrypoint", &[]) {
                Err(Error::RuntimeTerminated(term)) => {
                    assert_eq!(
                        *term
                            .provided_details()
                            .expect("user provided termination reason")
                            .downcast_ref::<&'static str>()
                            .expect("error was static str"),
                        ERROR_MESSAGE
                    );
                }
                res => panic!("unexpected result: {:?}", res),
            }

            assert!(NESTED_OUTER.is_poisoned());
            assert!(NESTED_INNER.is_poisoned());
        }

        /// Like `nested_error_unwind`, but the guest code callback in between the two segments of
        /// hostcall stack uses enough locals to require saving callee registers.
        #[test]
        fn nested_error_unwind_regs() {
            let module =
                test_module_c("host", "nested_error_unwind.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("entrypoint_regs", &[]) {
                Err(Error::RuntimeTerminated(term)) => {
                    assert_eq!(
                        *term
                            .provided_details()
                            .expect("user provided termination reason")
                            .downcast_ref::<&'static str>()
                            .expect("error was static str"),
                        ERROR_MESSAGE
                    );
                }
                res => panic!("unexpected result: {:?}", res),
            }

            assert!(NESTED_REGS_OUTER.is_poisoned());
            assert!(NESTED_REGS_INNER.is_poisoned());
        }

        #[test]
        fn run_fpe() {
            let module = test_module_c("host", "fpe.c").expect("build and load module");
            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("trigger_div_error", &[0u32.into()]) {
                Err(Error::RuntimeFault(details)) => {
                    assert_eq!(details.trapcode, Some(TrapCode::IntegerDivByZero));
                }
                res => {
                    panic!("unexpected result: {:?}", res);
                }
            }
        }

        #[test]
        fn run_hostcall_print_host_rsp() {
            extern "C" {
                fn hostcall_print_host_rsp(vmctx: *mut lucet_vmctx);
            }

            unsafe extern "C" fn f(vmctx: *mut lucet_vmctx) {
                hostcall_print_host_rsp(vmctx);
            }

            let module = MockModuleBuilder::new()
                .with_export_func(MockExportBuilder::new(
                    "f",
                    FunctionPointer::from_usize(f as usize),
                ))
                .build();

            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            inst.run("f", &[]).unwrap();
        }

        #[test]
        fn run_hostcall_bad_borrow() {
            extern "C" {
                fn hostcall_bad_borrow(vmctx: *mut lucet_vmctx) -> bool;
            }

            unsafe extern "C" fn f(vmctx: *mut lucet_vmctx) {
                hostcall_bad_borrow(vmctx);
            }

            let module = MockModuleBuilder::new()
                .with_export_func(MockExportBuilder::new(
                    "f",
                    FunctionPointer::from_usize(f as usize),
                ))
                .build();

            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("f", &[]) {
                Err(Error::RuntimeTerminated(details)) => {
                    assert_eq!(details, TerminationDetails::BorrowError("heap_mut"));
                }
                res => {
                    panic!("unexpected result: {:?}", res);
                }
            }
        }

        #[test]
        fn run_hostcall_missing_embed_ctx() {
            extern "C" {
                fn hostcall_missing_embed_ctx(vmctx: *mut lucet_vmctx) -> bool;
            }

            unsafe extern "C" fn f(vmctx: *mut lucet_vmctx) {
                hostcall_missing_embed_ctx(vmctx);
            }

            let module = MockModuleBuilder::new()
                .with_export_func(MockExportBuilder::new(
                    "f",
                    FunctionPointer::from_usize(f as usize),
                ))
                .build();

            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            match inst.run("f", &[]) {
                Err(Error::RuntimeTerminated(details)) => {
                    assert_eq!(details, TerminationDetails::CtxNotFound);
                }
                res => {
                    panic!("unexpected result: {:?}", res);
                }
            }
        }

        #[test]
        fn run_hostcall_multiple_vmctx() {
            extern "C" {
                fn hostcall_multiple_vmctx(vmctx: *mut lucet_vmctx) -> bool;
            }

            unsafe extern "C" fn f(vmctx: *mut lucet_vmctx) {
                hostcall_multiple_vmctx(vmctx);
            }

            let module = MockModuleBuilder::new()
                .with_export_func(MockExportBuilder::new(
                    "f",
                    FunctionPointer::from_usize(f as usize),
                ))
                .build();

            let region = TestRegion::create(1, &Limits::default()).expect("region can be created");
            let mut inst = region
                .new_instance(module)
                .expect("instance can be created");

            let retval = inst.run("f", &[]).expect("instance runs");
            assert_eq!(bool::from(retval), true);
        }
    };
}
