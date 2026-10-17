use bevy_reflect::Reflect;
use serde::{Deserialize, Serialize};

/// Why an end closed a link, which an IO layer carries to the peer where its protocol can, as
/// QUIC's application error code. The codes below [`AppCode::FIRST`] are Lightyear's own; an app
/// names its own from there up, and reads a peer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Reflect)]
#[serde(transparent)]
pub struct AppCode(u64);

impl AppCode {
    /// The link ended with no reason given, as a removed link entity ends it.
    pub const ENDED: Self = Self(0);
    /// The endpoint dropped, as its app exited.
    pub const STOPPED: Self = Self(1);
    /// The connection cannot carry the link's minimum MTU.
    pub const UNFIT: Self = Self(2);
    /// The first code an app may name.
    pub const FIRST: u32 = 16;

    /// The app's code `code`, `FIRST` or more.
    pub const fn new(code: u32) -> Self {
        assert!(
            code >= Self::FIRST,
            "the codes below FIRST are Lightyear's own"
        );
        Self(code as u64)
    }

    /// The code an IO layer read from the wire, Lightyear's own or an app's.
    pub const fn from_wire(code: u64) -> Self {
        Self(code)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app's code keeps its number, a code read from the wire keeps it below `FIRST` too, and
    /// Lightyear's own codes stay below `FIRST`.
    #[test]
    fn codes_keep_their_numbers() {
        assert_eq!(AppCode::new(AppCode::FIRST).get(), 16);
        assert_eq!(AppCode::new(40).get(), 40);
        assert_eq!(AppCode::from_wire(1), AppCode::STOPPED);
        assert_eq!(
            [AppCode::ENDED, AppCode::STOPPED, AppCode::UNFIT].map(AppCode::get),
            [0, 1, 2]
        );
    }

    #[test]
    #[should_panic(expected = "the codes below FIRST are Lightyear's own")]
    fn an_app_code_below_first_panics() {
        AppCode::new(15);
    }
}
