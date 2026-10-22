//! The coder's live fixture. Its test fails at this base on purpose: the
//! live coder check commits a patch making `one()` return 1 to a scratch
//! branch, and the fixture workflow's `verify` mode compiles and tests that
//! exact commit on a GitHub-hosted runner. It is not part of the workspace.

pub fn one() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_is_one() {
        assert_eq!(super::one(), 1);
    }
}
