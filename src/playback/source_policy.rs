//! Private fail-closed policy for the HTTP element created by `playbin3`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gst::glib;
use gst::prelude::*;
use gstreamer as gst;

use super::PlaybackFactory;

const SOURCE_SETUP_SIGNAL: &str = "source-setup";
const REJECTION_MESSAGE: &str = "balun-source-policy-rejected";
const USER_AGENT: &str = concat!("Balun/", env!("CARGO_PKG_VERSION"));
const TIMEOUT_SECONDS: u32 = 10;
const RETRIES: i32 = 0;

#[derive(Debug, Clone, Copy)]
pub(super) struct SourcePolicyError;

struct SourcePolicyState {
    expected_factory: gst::ElementFactory,
    #[cfg(test)]
    test_file_factory: Option<gst::ElementFactory>,
    accepted_source: Mutex<Option<AcceptedSource>>,
    rejected: AtomicBool,
}

struct AcceptedSource {
    object: gst::Object,
    trusted_http: bool,
}

pub(super) struct SourcePolicy {
    state: Arc<SourcePolicyState>,
    playbin: glib::WeakRef<gst::Pipeline>,
    signal_handler: Option<glib::SignalHandlerId>,
}

#[derive(Clone)]
pub(super) struct SourcePolicyMonitor {
    state: Arc<SourcePolicyState>,
}

impl SourcePolicy {
    pub(super) fn install(playbin: &gst::Pipeline) -> Result<Self, SourcePolicyError> {
        let expected_factory = gst::ElementFactory::find(PlaybackFactory::SoupHttpSource.name())
            .ok_or(SourcePolicyError)?;
        let preflight = expected_factory
            .create()
            .build()
            .map_err(|_| SourcePolicyError)?;
        if preflight.factory().as_ref() != Some(&expected_factory)
            || !configure_and_verify(&preflight)
        {
            return Err(SourcePolicyError);
        }

        let signal_id = validated_source_setup_signal(playbin)?;
        let state = Arc::new(SourcePolicyState {
            expected_factory,
            #[cfg(test)]
            test_file_factory: gst::ElementFactory::find("filesrc"),
            accepted_source: Mutex::new(None),
            rejected: AtomicBool::new(false),
        });
        let playbin_weak = playbin.downgrade();
        let callback_playbin = playbin_weak.clone();
        let callback_state = Arc::clone(&state);
        let signal_handler = playbin.connect_id(signal_id, None, false, move |args| {
            let playbin = callback_playbin.upgrade();
            let source = args
                .get(1)
                .and_then(|value| value.get::<gst::Element>().ok());
            let valid_emitter = playbin.as_ref().is_some_and(|expected| {
                args.first()
                    .and_then(|value| value.get::<gst::Pipeline>().ok())
                    .is_some_and(|emitter| emitter == *expected)
            });

            if args.len() != 2 || !valid_emitter {
                callback_state.reject(playbin.as_ref(), source.as_ref());
                return None;
            }
            let Some(source) = source else {
                callback_state.reject(playbin.as_ref(), None);
                return None;
            };
            callback_state.inspect_source(
                playbin
                    .as_ref()
                    .expect("a valid emitter retains the weak playbin"),
                &source,
            );
            None
        });

        Ok(Self {
            state,
            playbin: playbin_weak,
            signal_handler: Some(signal_handler),
        })
    }

    pub(super) fn is_rejected(&self) -> bool {
        self.state.rejected.load(Ordering::Acquire)
    }

    pub(super) fn monitor(&self) -> SourcePolicyMonitor {
        SourcePolicyMonitor {
            state: Arc::clone(&self.state),
        }
    }
}

impl SourcePolicyMonitor {
    pub(super) fn is_trusted_http_source(&self, source: &gst::Object) -> bool {
        if self.state.rejected.load(Ordering::Acquire) {
            return false;
        }
        let matches = self.state.accepted_source.lock().is_ok_and(|accepted| {
            !self.state.rejected.load(Ordering::Acquire)
                && accepted
                    .as_ref()
                    .is_some_and(|accepted| accepted.trusted_http && accepted.object == *source)
        });
        matches && !self.state.rejected.load(Ordering::Acquire)
    }
}

impl Drop for SourcePolicy {
    fn drop(&mut self) {
        let Some(signal_handler) = self.signal_handler.take() else {
            return;
        };
        if let Some(playbin) = self.playbin.upgrade() {
            playbin.disconnect(signal_handler);
        }
    }
}

impl SourcePolicyState {
    fn inspect_source(&self, playbin: &gst::Pipeline, source: &gst::Element) {
        if self.rejected.load(Ordering::Acquire) {
            self.reject(Some(playbin), Some(source));
            return;
        }

        let factory = source.factory();
        let production_source = factory.as_ref() == Some(&self.expected_factory);
        #[cfg(test)]
        let test_file_source = factory
            .as_ref()
            .is_some_and(|factory| self.test_file_factory.as_ref() == Some(factory));
        #[cfg(not(test))]
        let test_file_source = false;

        if (!production_source && !test_file_source)
            || (production_source && !configure_and_verify(source))
        {
            self.reject(Some(playbin), Some(source));
            return;
        }

        let source_object = source.clone().upcast::<gst::Object>();
        let accepted = self.accepted_source.lock();
        let Ok(mut accepted) = accepted else {
            self.reject(Some(playbin), Some(source));
            return;
        };
        if self.rejected.load(Ordering::Acquire) {
            drop(accepted);
            self.reject(Some(playbin), Some(source));
            return;
        }
        match accepted.as_ref() {
            None => {
                *accepted = Some(AcceptedSource {
                    object: source_object,
                    trusted_http: production_source,
                });
            }
            Some(existing) if existing.object == source_object => {}
            Some(_) => {
                drop(accepted);
                self.reject(Some(playbin), Some(source));
            }
        }
    }

    fn reject(&self, playbin: Option<&gst::Pipeline>, source: Option<&gst::Element>) {
        let first_rejection = !self.rejected.swap(true, Ordering::AcqRel);
        if let Some(source) = source {
            source.set_locked_state(true);
            let _ = source.set_state(gst::State::Null);
        }

        if !first_rejection {
            return;
        }
        let Some(playbin) = playbin else {
            return;
        };
        let marker = gst::Structure::builder(REJECTION_MESSAGE).build();
        let message = gst::message::Application::builder(marker)
            .src(playbin)
            .build();
        if let Some(bus) = playbin.bus() {
            let _ = bus.post(message);
        }
    }
}

fn validated_source_setup_signal(
    playbin: &gst::Pipeline,
) -> Result<glib::subclass::SignalId, SourcePolicyError> {
    let signal_id = glib::subclass::SignalId::lookup(SOURCE_SETUP_SIGNAL, playbin.type_())
        .ok_or(SourcePolicyError)?;
    let query = signal_id.query();
    let parameters = query.param_types();
    if query.signal_name() != SOURCE_SETUP_SIGNAL
        || query.return_type() != glib::Type::UNIT
        || parameters.len() != 1
        || parameters[0] != gst::Element::static_type()
    {
        return Err(SourcePolicyError);
    }
    Ok(signal_id)
}

fn readable_writable_property<T: glib::types::StaticType>(
    source: &gst::Element,
    name: &str,
) -> bool {
    source.find_property(name).is_some_and(|property| {
        let flags = property.flags();
        property.value_type() == T::static_type()
            && flags.contains(glib::ParamFlags::READABLE | glib::ParamFlags::WRITABLE)
            && !flags.contains(glib::ParamFlags::CONSTRUCT_ONLY)
    })
}

fn configure_and_verify(source: &gst::Element) -> bool {
    if !readable_writable_property::<bool>(source, "automatic-redirect")
        || !readable_writable_property::<String>(source, "proxy")
        || !readable_writable_property::<u32>(source, "timeout")
        || !readable_writable_property::<i32>(source, "retries")
        || !readable_writable_property::<bool>(source, "iradio-mode")
        || !readable_writable_property::<bool>(source, "compress")
        || !readable_writable_property::<String>(source, "user-agent")
    {
        return false;
    }
    let Some(log_property) = source.find_property("http-log-level").filter(|property| {
        let flags = property.flags();
        flags.contains(glib::ParamFlags::READABLE | glib::ParamFlags::WRITABLE)
            && !flags.contains(glib::ParamFlags::CONSTRUCT_ONLY)
    }) else {
        return false;
    };
    let Some(log_class) = glib::EnumClass::with_type(log_property.value_type()) else {
        return false;
    };
    let Some(log_none) = log_class.to_value_by_nick("none") else {
        return false;
    };

    source.set_property("automatic-redirect", false);
    // This clears an explicit element proxy. It does not promise that a
    // platform or libsoup resolver will bypass ambient proxy configuration.
    source.set_property("proxy", "");
    source.set_property("timeout", TIMEOUT_SECONDS);
    source.set_property("retries", RETRIES);
    source.set_property("iradio-mode", false);
    source.set_property("compress", false);
    source.set_property("user-agent", USER_AGENT);
    source.set_property_from_value("http-log-level", &log_none);

    let configured_log = source.property_value("http-log-level");
    let configured_log_is_none = glib::EnumValue::from_value(&configured_log)
        .is_some_and(|(_, value)| value.nick() == "none");
    !source.property::<bool>("automatic-redirect")
        && source.property::<String>("proxy").is_empty()
        && source.property::<u32>("timeout") == TIMEOUT_SECONDS
        && source.property::<i32>("retries") == RETRIES
        && !source.property::<bool>("iradio-mode")
        && !source.property::<bool>("compress")
        && source.property::<String>("user-agent") == USER_AGENT
        && configured_log_is_none
}

pub(super) fn is_rejection_message(message: &gst::MessageRef, playbin: &gst::Pipeline) -> bool {
    let gst::MessageView::Application(application) = message.view() else {
        return false;
    };
    message
        .src()
        .is_some_and(|source| source == playbin.upcast_ref::<gst::Object>())
        && application.structure().is_some_and(|structure| {
            structure.name() == REJECTION_MESSAGE && structure.n_fields() == 0
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline() -> Option<gst::Pipeline> {
        gst::init().ok()?;
        gst::ElementFactory::make("playbin3")
            .build()
            .ok()?
            .downcast::<gst::Pipeline>()
            .ok()
    }

    #[test]
    fn accepted_source_schema_and_configuration_are_network_free() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin) else {
            return;
        };
        assert!(validated_source_setup_signal(&playbin).is_ok());
        let source = gst::ElementFactory::make("souphttpsrc").build().unwrap();

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);

        assert!(!policy.is_rejected());
        assert!(configure_and_verify(&source));
        assert_eq!(
            policy
                .state
                .accepted_source
                .lock()
                .unwrap()
                .as_ref()
                .map(|accepted| &accepted.object),
            Some(source.upcast_ref::<gst::Object>())
        );
        assert!(
            policy
                .state
                .accepted_source
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|accepted| accepted.trusted_http)
        );
    }

    #[test]
    fn rejection_marker_is_field_free_and_deduplicated() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin) else {
            return;
        };
        let bus = playbin.bus().unwrap();
        bus.set_flushing(false);
        let first = gst::ElementFactory::make("fakesrc").build().unwrap();
        let second = gst::ElementFactory::make("fakesrc").build().unwrap();

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&first]);
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&second]);

        assert!(policy.is_rejected());
        assert!(first.is_locked_state());
        assert!(second.is_locked_state());
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_mseconds(10),
                &[gst::MessageType::Application],
            )
            .expect("the first rejection posts its fixed marker");
        assert!(is_rejection_message(&message, &playbin));
        assert!(
            bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Application])
                .is_none()
        );
    }

    #[test]
    fn source_setup_callback_is_safe_from_a_worker_thread() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin) else {
            return;
        };
        let worker_playbin = playbin.clone();
        let source = std::thread::spawn(move || {
            let source = gst::ElementFactory::make("souphttpsrc").build().unwrap();
            worker_playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
            source
        })
        .join()
        .unwrap();

        assert!(!policy.is_rejected());
        assert_eq!(
            policy
                .state
                .accepted_source
                .lock()
                .unwrap()
                .as_ref()
                .map(|accepted| &accepted.object),
            Some(source.upcast_ref::<gst::Object>())
        );
    }

    #[test]
    fn test_only_file_source_is_accepted_but_never_http_trusted() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin) else {
            return;
        };
        let Ok(source) = gst::ElementFactory::make("filesrc").build() else {
            return;
        };

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);

        assert!(!policy.is_rejected());
        assert!(
            !policy
                .monitor()
                .is_trusted_http_source(source.upcast_ref::<gst::Object>())
        );
    }

    #[test]
    fn later_source_rejection_revokes_the_previously_trusted_identity() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin) else {
            return;
        };
        let source = gst::ElementFactory::make("souphttpsrc").build().unwrap();
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
        let monitor = policy.monitor();
        assert!(monitor.is_trusted_http_source(source.upcast_ref::<gst::Object>()));

        let unexpected = gst::ElementFactory::make("fakesrc").build().unwrap();
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&unexpected]);

        assert!(policy.is_rejected());
        assert!(unexpected.is_locked_state());
        assert!(!monitor.is_trusted_http_source(source.upcast_ref::<gst::Object>()));
    }
}
