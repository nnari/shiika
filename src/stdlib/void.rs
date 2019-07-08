use crate::names::*;
use crate::hir::*;
//use crate::stdlib::create_method;

pub fn create_class() -> SkClass {
    SkClass {
        fullname: ClassFullname("Void".to_string()),
        methods: create_methods(),
    }
}

fn create_methods() -> Vec<SkMethod> {
    vec![]
}

