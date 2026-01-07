#![no_std]
#![no_main]

panda_kernel::test_harness!(trivial_assertion, trivial_assertion_2);

fn trivial_assertion() {
    assert_eq!(1, 1);
}

fn trivial_assertion_2() {
    assert_eq!(2 + 2, 4);
}
