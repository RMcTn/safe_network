// Copyright 2024 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod appender;
mod error;
mod layers;
#[cfg(feature = "process-metrics")]
pub mod metrics;

use crate::error::Result;
use layers::TracingLayers;
use std::path::PathBuf;
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_core::{dispatcher::DefaultGuard, Level};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt};

pub use error::Error;
pub use layers::ReloadHandle;

#[derive(Debug, Clone)]
pub enum LogOutputDest {
    Stdout,
    Path(PathBuf),
}

impl std::fmt::Display for LogOutputDest {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            LogOutputDest::Stdout => write!(f, "stdout"),
            LogOutputDest::Path(p) => write!(f, "{}", p.to_string_lossy()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    Default,
    Json,
}

impl LogFormat {
    pub fn parse_from_str(val: &str) -> Result<Self> {
        match val {
            "default" => Ok(LogFormat::Default),
            "json" => Ok(LogFormat::Json),
            _ => Err(Error::LoggingConfiguration(
                "The only valid values for this argument are \"default\" or \"json\"".to_string(),
            )),
        }
    }
}

pub struct LogBuilder {
    default_logging_targets: Vec<(String, Level)>,
    output_dest: LogOutputDest,
    format: LogFormat,
    max_uncompressed_log_files: Option<usize>,
    max_compressed_log_files: Option<usize>,
}

impl LogBuilder {
    /// Create a new builder
    /// Provide the default_logging_targets that are used if the `SN_LOG` env variable is not set.
    ///
    /// By default, we use log to the StdOut with the default format.
    pub fn new(default_logging_targets: Vec<(String, Level)>) -> Self {
        Self {
            default_logging_targets,
            output_dest: LogOutputDest::Stdout,
            format: LogFormat::Default,
            max_uncompressed_log_files: None,
            max_compressed_log_files: None,
        }
    }

    /// Set the logging output destination
    pub fn output_dest(&mut self, output_dest: LogOutputDest) {
        self.output_dest = output_dest;
    }

    /// Set the logging format
    pub fn format(&mut self, format: LogFormat) {
        self.format = format
    }

    /// The max number of uncompressed log files to store
    pub fn max_uncompressed_log_files(&mut self, files: usize) {
        self.max_uncompressed_log_files = Some(files);
    }

    /// The max number of compressed files to store
    pub fn max_compressed_log_files(&mut self, files: usize) {
        self.max_compressed_log_files = Some(files);
    }

    /// Inits node logging, returning the NonBlocking guard if present.
    /// This guard should be held for the life of the program.
    ///
    /// Logging should be instantiated only once.
    pub fn initialize(self) -> Result<(ReloadHandle, Option<WorkerGuard>)> {
        let mut layers = TracingLayers::default();

        let reload_handle = layers.fmt_layer(
            self.default_logging_targets.clone(),
            &self.output_dest,
            self.format,
            self.max_uncompressed_log_files,
            self.max_compressed_log_files,
        )?;

        #[cfg(feature = "otlp")]
        {
            match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
                Ok(_) => layers.otlp_layer(self.default_logging_targets)?,
                Err(_) => println!(
                "The OTLP feature is enabled but the OTEL_EXPORTER_OTLP_ENDPOINT variable is not \
                set, so traces will not be submitted."
            ),
            }
        }

        if tracing_subscriber::registry()
            .with(layers.layers)
            .try_init()
            .is_err()
        {
            println!("Tried to initialize and set global default subscriber more than once");
        }

        Ok((reload_handle, layers.log_appender_guard))
    }

    /// Logs to the data_dir. Should be called from a single threaded tokio/non-tokio context.
    /// Provide the test file name to capture tracings from the test.
    ///
    /// subscriber.set_default() should be used if under a single threaded tokio / single threaded non-tokio context.
    /// Refer here for more details: <https://github.com/tokio-rs/tracing/discussions/1626>
    pub fn init_single_threaded_tokio_test(
        test_file_name: &str,
    ) -> (Option<WorkerGuard>, DefaultGuard) {
        let layers = Self::get_test_layers(test_file_name);
        let log_guard = tracing_subscriber::registry()
            .with(layers.layers)
            .set_default();
        // this is the test_name and not the test_file_name
        if let Some(test_name) = std::thread::current().name() {
            info!("Running test: {test_name}");
        }
        (layers.log_appender_guard, log_guard)
    }

    /// Logs to the data_dir. Should be called from a multi threaded tokio context.
    /// Provide the test file name to capture tracings from the test.
    ///
    /// subscriber.init() should be used under multi threaded tokio context. If you have 1+ multithreaded tokio tests under
    /// the same integration test, this might result in loss of logs. Hence use .init() (instead of .try_init()) to panic
    /// if called more than once.
    pub fn init_multi_threaded_tokio_test(test_file_name: &str) -> Option<WorkerGuard> {
        let layers = Self::get_test_layers(test_file_name);
        tracing_subscriber::registry()
        .with(layers.layers)
        .try_init()
        .expect("You have tried to init multi_threaded tokio logging twice\nRefer sn_logging::get_test_layers docs for more.");

        layers.log_appender_guard
    }

    /// Initialize just the fmt_layer for testing purposes.
    ///
    /// Also overwrites the SN_LOG variable to log everything including the test_file_name
    fn get_test_layers(test_file_name: &str) -> TracingLayers {
        // overwrite SN_LOG
        std::env::set_var("SN_LOG", format!("{test_file_name}=TRACE,all"));

        let output_dest = match dirs_next::data_dir() {
            Some(dir) => {
                // Get the current timestamp and format it to be human readable
                let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
                let path = dir
                    .join("safe")
                    .join("client")
                    .join("logs")
                    .join(format!("log_{timestamp}"));
                LogOutputDest::Path(path)
            }
            None => LogOutputDest::Stdout,
        };

        let mut layers = TracingLayers::default();

        let _reload_handle = layers
            .fmt_layer(vec![], &output_dest, LogFormat::Default, None, None)
            .expect("Failed to get TracingLayers");
        layers
    }
}

#[cfg(test)]
mod tests {
    use crate::{layers::LogFormatter, ReloadHandle};
    use color_eyre::Result;
    use tracing::{trace, warn, Level};
    use tracing_subscriber::{
        filter::Targets,
        fmt as tracing_fmt,
        layer::{Filter, SubscriberExt},
        reload,
        util::SubscriberInitExt,
        Layer, Registry,
    };
    use tracing_test::internal::GLOBAL_BUF;

    #[test]
    // todo: break down the TracingLayers so that we can plug in the writer without having to rewrite the whole function
    // here.
    fn reload_handle_should_change_log_levels() -> Result<()> {
        // A mock write that writes to stdout + collects events to a global buffer. We can later read from this buffer.
        let mock_writer = tracing_test::internal::MockWriter::new(&GLOBAL_BUF);

        // Constructing the fmt layer manually.
        let layer = tracing_fmt::layer()
            .with_ansi(false)
            .with_target(false)
            .event_format(LogFormatter)
            .with_writer(mock_writer)
            .boxed();

        let test_target = "sn_logging::tests".to_string();
        // to enable logs just for the test.
        let target_filters: Box<dyn Filter<Registry> + Send + Sync> =
            Box::new(Targets::new().with_targets(vec![(test_target.clone(), Level::TRACE)]));

        // add the reload layer
        let (filter, handle) = reload::Layer::new(target_filters);
        let reload_handle = ReloadHandle(handle);
        let layer = layer.with_filter(filter);
        tracing_subscriber::registry().with(layer).try_init()?;

        // Span is not controlled by the ReloadHandle. So we can set any span here.
        let _span = tracing::info_span!("info span");

        trace!("First trace event");

        {
            let buf = GLOBAL_BUF.lock().unwrap();

            let events: Vec<&str> = std::str::from_utf8(&buf)
                .expect("Logs contain invalid UTF8")
                .lines()
                .collect();
            assert_eq!(events.len(), 1);
            assert!(events[0].contains("First trace event"));
        }

        reload_handle.modify_log_level("sn_logging::tests=WARN")?;

        // trace should not be logged now.
        trace!("Second trace event");
        warn!("First warn event");

        {
            let buf = GLOBAL_BUF.lock().unwrap();

            let events: Vec<&str> = std::str::from_utf8(&buf)
                .expect("Logs contain invalid UTF8")
                .lines()
                .collect();

            assert_eq!(events.len(), 2);
            assert!(events[0].contains("First trace event"));
            assert!(events[1].contains("First warn event"));
        }

        Ok(())
    }
}
