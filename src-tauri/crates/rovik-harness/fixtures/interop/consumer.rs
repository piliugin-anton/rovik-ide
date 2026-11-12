use std::ffi::c_void;
#[repr(C)]
struct Report {
    checksum: u64,
    size: usize,
}
unsafe extern "C" {
    fn packet_create(size: usize, seed: u64, out: *mut *mut c_void) -> i32;
    fn packet_read(
        value: *const c_void,
        out: *mut Report,
        callback: Option<unsafe extern "C" fn(*mut c_void, u64)>,
        context: *mut c_void,
    ) -> i32;
    fn packet_destroy(value: *mut c_void);
    fn packet_report_size() -> usize;
    fn packet_report_size_offset() -> usize;
    fn harness_reset(failure: i32);
    fn harness_live() -> i32;
}
unsafe extern "C" fn observed(context: *mut c_void, checksum: u64) {
    unsafe {
        *(context.cast::<u64>()) += checksum;
    }
}
fn main() {
    unsafe {
        assert_eq!(packet_report_size(), std::mem::size_of::<Report>());
        assert_eq!(
            packet_report_size_offset(),
            std::mem::offset_of!(Report, size)
        );
        for failure in 1..=3 {
            harness_reset(failure);
            let mut value = 1usize as *mut c_void;
            assert_eq!(packet_create(4, 3, &mut value), 2);
            assert_eq!(value, 1usize as *mut c_void);
            assert_eq!(harness_live(), 0);
        }
        harness_reset(0);
        let mut value = std::ptr::null_mut();
        assert_eq!(packet_create(4, 3, &mut value), 0);
        assert!(!value.is_null());
        assert_eq!(harness_live(), 3);
        let mut callback = 0u64;
        let mut report = Report {
            checksum: 0,
            size: 0,
        };
        assert_eq!(
            packet_read(
                value,
                &mut report,
                Some(observed),
                (&mut callback as *mut u64).cast()
            ),
            0
        );
        assert_eq!((report.checksum, report.size, callback), (28, 4, 28));
        assert_eq!(
            packet_read(
                std::ptr::null(),
                &mut report,
                Some(observed),
                (&mut callback as *mut u64).cast()
            ),
            1
        );
        assert_eq!((report.checksum, report.size, callback), (28, 4, 28));
        assert_eq!(packet_create(0, 0, &mut value), 1);
        packet_destroy(value);
        assert_eq!(harness_live(), 0);
        assert_eq!(callback, 28);
        packet_destroy(std::ptr::null_mut());
    }
}
