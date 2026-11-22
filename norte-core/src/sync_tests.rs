use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use super::lock_or_recover;

#[test]
fn a_poisoned_mutex_still_yields_its_data() {
    let mutex = Mutex::new(7);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _guard = mutex.lock().expect("first lock");
        panic!("poison the mutex");
    }));
    assert!(panic.is_err());

    assert!(mutex.is_poisoned());
    assert_eq!(*lock_or_recover(&mutex), 7);
}
