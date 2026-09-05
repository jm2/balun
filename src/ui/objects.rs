//! Small GObject wrappers used by Balun's virtualized sidebar models.
//!
//! These objects are constructed only on the GTK main thread from bounded,
//! URL-free controller projections. Stable domain identity is retained
//! separately from presentation text so a recycled list row or changed model
//! position can never become identity.

use std::cell::{Cell, RefCell};

use balun::controller::{ChannelSummary, DeviceSummary};
use balun::domain::{ChannelKey, DeviceId};
use gtk::glib;
use gtk::subclass::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeviceRowProjection {
    device_id: DeviceId,
    title: String,
    subtitle: String,
}

impl DeviceRowProjection {
    fn from_summary(summary: &DeviceSummary) -> Self {
        let primary_name = summary
            .friendly_name()
            .or(summary.model_number())
            .unwrap_or("HDHomeRun");
        let title = format!("{primary_name} · {}", summary.device_id());

        let mut details = Vec::with_capacity(4);
        if let Some(model) = summary
            .model_number()
            .filter(|model| *model != primary_name)
        {
            details.push(model.to_owned());
        }
        if let Some(count) = summary.tuner_count() {
            details.push(format!(
                "{count} tuner{}",
                if count == 1 { "" } else { "s" }
            ));
        }
        details.push(summary.preferred_locator().to_string());
        if summary.locator_count() > 1 {
            details.push(format!("{} addresses", summary.locator_count()));
        }

        Self {
            device_id: summary.device_id(),
            title,
            subtitle: details.join(" · "),
        }
    }
}

mod device_imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct DeviceRowObject {
        pub device_id: Cell<Option<DeviceId>>,
        pub title: RefCell<String>,
        pub subtitle: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DeviceRowObject {
        const NAME: &'static str = "BalunDeviceRowObject";
        type Type = super::DeviceRowObject;
    }

    impl ObjectImpl for DeviceRowObject {}
}

glib::wrapper! {
    /// One URL-free row in the HDHomeRun device model.
    pub struct DeviceRowObject(ObjectSubclass<device_imp::DeviceRowObject>);
}

impl DeviceRowObject {
    pub(crate) fn from_summary(summary: &DeviceSummary) -> Self {
        let projection = DeviceRowProjection::from_summary(summary);
        let object: Self = glib::Object::builder().build();
        object.imp().device_id.set(Some(projection.device_id));
        object.imp().title.replace(projection.title);
        object.imp().subtitle.replace(projection.subtitle);
        object
    }

    #[must_use]
    pub(crate) fn device_id(&self) -> Option<DeviceId> {
        self.imp().device_id.get()
    }

    #[must_use]
    pub(crate) fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }

    #[must_use]
    pub(crate) fn subtitle(&self) -> String {
        self.imp().subtitle.borrow().clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChannelRowProjection {
    key: ChannelKey,
    number: String,
    name: String,
    favorite: bool,
    drm: bool,
    hd: bool,
}

impl ChannelRowProjection {
    fn from_summary(summary: &ChannelSummary) -> Self {
        Self {
            key: summary.key().clone(),
            number: summary.key().guide_number().as_str().to_owned(),
            name: summary.name().to_owned(),
            favorite: summary.is_favorite(),
            drm: summary.is_drm(),
            hd: summary.is_hd(),
        }
    }
}

mod channel_imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ChannelRowObject {
        pub key: RefCell<Option<ChannelKey>>,
        pub number: RefCell<String>,
        pub name: RefCell<String>,
        pub favorite: Cell<bool>,
        pub drm: Cell<bool>,
        pub hd: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChannelRowObject {
        const NAME: &'static str = "BalunChannelRowObject";
        type Type = super::ChannelRowObject;
    }

    impl ObjectImpl for ChannelRowObject {}
}

glib::wrapper! {
    /// One URL-free row in the selected device's channel model.
    pub struct ChannelRowObject(ObjectSubclass<channel_imp::ChannelRowObject>);
}

impl ChannelRowObject {
    pub(crate) fn from_summary(summary: &ChannelSummary) -> Self {
        let projection = ChannelRowProjection::from_summary(summary);
        let object: Self = glib::Object::builder().build();
        object.imp().key.replace(Some(projection.key));
        object.imp().number.replace(projection.number);
        object.imp().name.replace(projection.name);
        object.imp().favorite.set(projection.favorite);
        object.imp().drm.set(projection.drm);
        object.imp().hd.set(projection.hd);
        object
    }

    /// Whether this row already shows exactly `summary`, so a model that
    /// matches its lineup row for row can be left alone.
    #[must_use]
    pub(crate) fn matches(&self, summary: &ChannelSummary) -> bool {
        self.key()
            .map(|key| ChannelRowProjection {
                key,
                number: self.number(),
                name: self.name(),
                favorite: self.is_favorite(),
                drm: self.is_drm(),
                hd: self.is_hd(),
            })
            .is_some_and(|projection| projection == ChannelRowProjection::from_summary(summary))
    }

    #[must_use]
    pub(crate) fn key(&self) -> Option<ChannelKey> {
        self.imp().key.borrow().clone()
    }

    #[must_use]
    pub(crate) fn number(&self) -> String {
        self.imp().number.borrow().clone()
    }

    #[must_use]
    pub(crate) fn name(&self) -> String {
        self.imp().name.borrow().clone()
    }

    #[must_use]
    pub(crate) fn is_favorite(&self) -> bool {
        self.imp().favorite.get()
    }

    #[must_use]
    pub(crate) fn is_drm(&self) -> bool {
        self.imp().drm.get()
    }

    #[must_use]
    pub(crate) fn is_hd(&self) -> bool {
        self.imp().hd.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use balun::controller::{ChannelSummary, DeviceSummary};
    use balun::domain::GuideNumber;

    fn device_id() -> DeviceId {
        DeviceId::new(0x105A_1232).unwrap()
    }

    #[test]
    fn channel_row_matches_only_an_identical_summary() {
        let key = ChannelKey::new(device_id(), GuideNumber::new("5.1").unwrap());
        let summary =
            ChannelSummary::new(key.clone(), "News".to_owned(), true, false, true).unwrap();
        let row = ChannelRowObject::from_summary(&summary);
        assert!(row.matches(&summary));
        let unfavorited = ChannelSummary::new(key, "News".to_owned(), false, false, true).unwrap();
        assert!(!row.matches(&unfavorited));
    }

    #[test]
    fn device_projection_keeps_identity_separate_from_display_text() {
        let summary = DeviceSummary::new(
            device_id(),
            Some("Living room".to_owned()),
            Some("HDHR5-4K".to_owned()),
            Some(4),
            "192.0.2.10:65001".parse().unwrap(),
            vec![
                "192.0.2.10:65001".parse().unwrap(),
                "[2001:db8::10]:65001".parse().unwrap(),
            ],
        )
        .unwrap();

        let row = DeviceRowProjection::from_summary(&summary);

        assert_eq!(row.device_id, device_id());
        assert_eq!(row.title, "Living room · 105A1232");
        assert_eq!(
            row.subtitle,
            "HDHR5-4K · 4 tuners · 192.0.2.10:65001 · 2 addresses"
        );
    }

    #[test]
    fn channel_projection_retains_device_scoped_key_and_flags() {
        let key = ChannelKey::new(device_id(), GuideNumber::new("7.1").unwrap());
        let summary =
            ChannelSummary::new(key.clone(), "Synthetic News".to_owned(), true, false, true)
                .unwrap();

        let row = ChannelRowProjection::from_summary(&summary);

        assert_eq!(row.key, key);
        assert_eq!(row.number, "7.1");
        assert_eq!(row.name, "Synthetic News");
        assert!(row.favorite);
        assert!(!row.drm);
        assert!(row.hd);
    }

    #[test]
    fn gobject_rows_round_trip_their_stable_identity_and_display_fields() {
        let device = DeviceSummary::new(
            device_id(),
            Some("Living room".to_owned()),
            Some("HDHR5-4K".to_owned()),
            Some(4),
            "192.0.2.10:65001".parse().unwrap(),
            vec!["192.0.2.10:65001".parse().unwrap()],
        )
        .unwrap();
        let device_row = DeviceRowObject::from_summary(&device);
        assert_eq!(device_row.device_id(), Some(device_id()));
        assert_eq!(device_row.title(), "Living room · 105A1232");
        assert!(device_row.subtitle().contains("HDHR5-4K"));

        let key = ChannelKey::new(device_id(), GuideNumber::new("7.1").unwrap());
        let channel =
            ChannelSummary::new(key.clone(), "Synthetic News".to_owned(), true, true, true)
                .unwrap();
        let channel_row = ChannelRowObject::from_summary(&channel);
        assert_eq!(channel_row.key(), Some(key));
        assert_eq!(channel_row.number(), "7.1");
        assert_eq!(channel_row.name(), "Synthetic News");
        assert!(channel_row.is_favorite());
        assert!(channel_row.is_drm());
        assert!(channel_row.is_hd());
    }
}
