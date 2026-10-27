//! A data client that plays bars through the live node at a steady pace, so paper trading can
//! be rehearsed (and tested) without a network.

use std::{any::Any, cell::RefCell, rc::Rc, time::Duration};

use anyhow::Context;
use async_trait::async_trait;
use nautilus_common::{
    cache::CacheView,
    clients::DataClient,
    clock::Clock,
    factories::{ClientConfig, DataClientFactory},
    live::runner::get_data_event_sender,
    messages::{
        DataEvent,
        data::{SubscribeBars, UnsubscribeBars},
    },
};
use nautilus_model::{
    data::{Bar, Data},
    identifiers::{ClientId, Venue},
    instruments::{Instrument, InstrumentAny},
};

#[derive(Debug)]
pub(crate) struct ReplayConfig {
    pub(crate) instrument: InstrumentAny,
    /// Oldest first, each stamped with its close time.
    pub(crate) bars: Vec<Bar>,
    /// How long to wait before each bar.
    pub(crate) every: Duration,
}

impl ClientConfig for ReplayConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
pub(crate) struct ReplayFactory;

impl DataClientFactory for ReplayFactory {
    fn create(
        &self,
        _name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<ReplayConfig>()
            .context("the replay feed needs a ReplayConfig")?;
        Ok(Box::new(ReplayClient {
            id: ClientId::new("REPLAY"),
            venue: config.instrument.id().venue,
            instrument: config.instrument.clone(),
            bars: config.bars.clone(),
            every: config.every,
            connected: false,
            task: None,
        }))
    }

    fn name(&self) -> &'static str {
        "REPLAY"
    }

    fn config_type(&self) -> &'static str {
        "ReplayConfig"
    }
}

struct ReplayClient {
    id: ClientId,
    venue: Venue,
    instrument: InstrumentAny,
    bars: Vec<Bar>,
    every: Duration,
    connected: bool,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ReplayClient {
    fn halt(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[async_trait(?Send)]
impl DataClient for ReplayClient {
    fn client_id(&self) -> ClientId {
        self.id
    }

    fn venue(&self) -> Option<Venue> {
        Some(self.venue)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.halt();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        self.halt();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn is_disconnected(&self) -> bool {
        !self.connected
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        get_data_event_sender()
            .send(DataEvent::Instrument(self.instrument.clone()))
            .map_err(|_| anyhow::anyhow!("the node stopped listening"))?;
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.halt();
        self.connected = false;
        Ok(())
    }

    fn subscribe_bars(&mut self, cmd: SubscribeBars) -> anyhow::Result<()> {
        let bars = std::mem::take(&mut self.bars);
        anyhow::ensure!(
            bars.first().is_none_or(|bar| bar.bar_type == cmd.bar_type),
            "the replay has {} bars, not {}",
            bars[0].bar_type,
            cmd.bar_type
        );
        let sender = get_data_event_sender();
        let every = self.every;
        self.task = Some(tokio::spawn(async move {
            for bar in bars {
                tokio::time::sleep(every).await;
                if sender.send(DataEvent::Data(Data::Bar(bar))).is_err() {
                    return;
                }
            }
        }));
        Ok(())
    }

    fn unsubscribe_bars(&mut self, _cmd: &UnsubscribeBars) -> anyhow::Result<()> {
        self.halt();
        Ok(())
    }
}
