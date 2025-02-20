//! Helper methods for [`glib::KeyFile`].

use glib::GString;
use ostree::glib;

/// Helper methods for [`glib::KeyFile`].
pub trait KeyFileExt {
    /// Get a string value, but return `None` if the key does not exist.
    fn optional_string(&self, group: &str, key: &str) -> Result<Option<GString>, glib::Error>;
    /// Get a boolean value, but return `None` if the key does not exist.
    fn optional_bool(&self, group: &str, key: &str) -> Result<Option<bool>, glib::Error>;
}

/// Consume a keyfile error, mapping the case where group or key is not found to `Ok(None)`.
pub fn map_keyfile_optional<T>(res: Result<T, glib::Error>) -> Result<Option<T>, glib::Error> {
    match res {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            match e.kind::<glib::KeyFileError>() { Some(t) => {
                match t {
                    glib::KeyFileError::GroupNotFound | glib::KeyFileError::KeyNotFound => Ok(None),
                    _ => Err(e),
                }
            } _ => {
                Err(e)
            }}
        }
    }
}

impl KeyFileExt for glib::KeyFile {
    fn optional_string(&self, group: &str, key: &str) -> Result<Option<GString>, glib::Error> {
        map_keyfile_optional(self.string(group, key))
    }

    fn optional_bool(&self, group: &str, key: &str) -> Result<Option<bool>, glib::Error> {
        map_keyfile_optional(self.boolean(group, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_optional() {
        let kf = glib::KeyFile::new();
        assert_eq!(kf.optional_string("foo", "bar").unwrap(), None);
        kf.set_string("foo", "baz", "someval");
        assert_eq!(kf.optional_string("foo", "bar").unwrap(), None);
        assert_eq!(
            kf.optional_string("foo", "baz").unwrap().unwrap(),
            "someval"
        );

        assert!(kf.optional_bool("foo", "baz").is_err());
        assert_eq!(kf.optional_bool("foo", "bar").unwrap(), None);
        kf.set_boolean("foo", "somebool", false);
        assert_eq!(kf.optional_bool("foo", "somebool").unwrap(), Some(false));
    }
}
