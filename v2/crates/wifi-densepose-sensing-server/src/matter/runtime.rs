//! Presence-only Matter bridge on rs-matter (ADR-431 spike).
//!
//! Built only with the `matter` feature. Endpoint layout:
//! - 0: root node (rs-matter's Ethernet root endpoint)
//! - 1: Aggregator, which lists the bridged endpoints
//! - 2: the room, an Occupancy Sensor that is also a Bridged Node
//!
//! The room's occupancy comes from [`OccupancyFeed`] over the server's
//! broadcast. The Matter stack runs on its own thread with its own executor,
//! so it never blocks the server's tokio runtime. The two share only a
//! [`RoomOccupancy`], which the handlers poll every [`POLL`] and report to
//! subscribed controllers when it changes.
//!
//! Spike limits, as ADR-431 sets them: CSA test vendor ID and test device
//! attestation, on-network pairing only (no BLE), the default Matter port
//! 5540, and rs-matter's built-in mDNS responder on port 5353.

use core::mem::MaybeUninit;
use core::pin::pin;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use embassy_time::{Duration, Timer};
use tokio::sync::broadcast;

use rs_matter::crypto::{default_crypto, Crypto as _, Rng};
use rs_matter::dm::clusters::decl::bridged_device_basic_information::{
    self as bdbi, ClusterHandler as _, KeepActiveRequest,
};
use rs_matter::dm::clusters::decl::occupancy_sensing::{
    self as occ, ClusterHandler as _, OccupancyBitmap, OccupancySensorTypeBitmap,
    OccupancySensorTypeEnum,
};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::devices::test::{
    DAC_PRIVKEY, TEST_BASIC_COMM, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET,
};
use rs_matter::dm::devices::{
    DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_OCCUPANCY_SENSOR,
};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{
    Async, Cluster, DataModel, Dataver, EmptyHandler, Endpoint, EndptId, HandlerContext,
    InvokeContext, Node, ReadContext, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, NoOptionalData, QrPayload};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::DirKvBlobStore;
use rs_matter::stack::eth::PreexistingEthernet;
use rs_matter::stack::net::std::StdNetStack;
use rs_matter::stack::MatterStack;
use rs_matter::tlv::{TLVBuilderParent, Utf8Str, Utf8StrBuilder};
use rs_matter::transport::network::mdns::builtin::BuiltinMdns;
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::{clusters, devices, root_endpoint, with, Matter, MATTER_PORT};

use super::config::BridgeConfig;
use super::occupancy::{spawn_feed, RoomOccupancy};

/// The aggregator endpoint.
pub const AGGREGATOR_EP: EndptId = 1;
/// The room's occupancy endpoint.
pub const ROOM_EP: EndptId = 2;

/// How often the handlers look at [`RoomOccupancy`] for a change. Well inside
/// ADR-431's 2 s reporting target.
const POLL: Duration = Duration::from_millis(100);

/// Bump allocator for the stack's futures. rs-matter panics at start if it is
/// too small; an unoptimised build needs more than a release build.
const BUMP_SIZE: usize = 64 * 1024;

type Stack = MatterStack<'static, BUMP_SIZE, EthNetwork<'static>>;

/// The name controllers first show for the room. They can rename it
/// (NodeLabel is writable); the new name lasts until the bridge restarts.
const ROOM_LABEL: &str = "RuView room";

/// Start the bridge: a tokio task that follows the room's occupancy, and a
/// thread that runs the Matter stack. Call from inside the tokio runtime.
pub fn start(cfg: BridgeConfig, rx: broadcast::Receiver<String>) -> std::io::Result<()> {
    let room = Arc::new(RoomOccupancy::default());
    spawn_feed(rx, room.clone());

    std::thread::Builder::new()
        .name("matter-bridge".into())
        // rs-matter's futures are large, more so in a debug build.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            if let Err(e) = run(cfg, room) {
                // `Display`, not `Debug`: rs-matter's `Debug` carries a long
                // backtrace. The server itself keeps running.
                let hint = if e.code() == ErrorCode::StdIoError {
                    " (Matter needs IPv6 on this host and UDP port 5540 free)"
                } else {
                    ""
                };
                tracing::error!("Matter bridge stopped: {e}{hint}");
            }
        })?;
    Ok(())
}

fn run(cfg: BridgeConfig, room: Arc<RoomOccupancy>) -> Result<(), Error> {
    if cfg.reset {
        match std::fs::remove_dir_all(&cfg.storage_dir) {
            Ok(()) => tracing::warn!(
                "Matter: --matter-reset removed the paired controllers in {}",
                cfg.storage_dir.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::error!("Matter: could not reset {}: {e}", cfg.storage_dir.display());
                return Err(ErrorCode::StdIoError.into());
            }
        }
    }
    std::fs::create_dir_all(&cfg.storage_dir).map_err(|e| {
        tracing::error!(
            "Matter: could not create {}: {e}",
            cfg.storage_dir.display()
        );
        Error::from(ErrorCode::StdIoError)
    })?;

    // The stack is large, so it is built in place on the heap. It lives as
    // long as the process, as the bridge does.
    let slot: &'static mut MaybeUninit<Stack> = Box::leak(Box::new_uninit());
    let stack: &'static Stack = slot.init_with(MatterStack::init_with_port(
        &TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        MATTER_PORT,
    ));

    let mut store = DirKvBlobStore::new(cfg.storage_dir.clone());
    let crypto = default_crypto(rand_010::rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;

    // Re-load the paired controllers. If there are none, this opens the
    // pairing window and logs the QR text and the manual pairing code.
    futures_lite::future::block_on(stack.startup(&crypto, &mut store))?;
    if stack.matter().has_fabrics() {
        tracing::info!("Matter: already paired; add another controller from the first one");
    } else if let Some(path) = &cfg.setup_file {
        match write_setup_file(path) {
            Ok(()) => tracing::info!("Matter: setup code written to {}", path.display()),
            Err(e) => tracing::warn!("Matter: could not write {}: {e}", path.display()),
        }
    }
    let kv = stack.kv(store);

    let occupancy = OccupancyHandler::new(Dataver::new_rand(&mut rand), ROOM_EP, room.clone());
    let bridged = RoomBridgedHandler::new(Dataver::new_rand(&mut rand), ROOM_EP, room);

    tracing::info!(
        "Matter bridge starting on UDP {MATTER_PORT} (room occupancy on endpoint {ROOM_EP})"
    );
    let matter = pin!(stack.run_eth(
        PreexistingEthernet::new(StdNetStack::new(), SysNetifs, &(), BuiltinMdns::new()),
        &crypto,
        data_model(stack.matter(), rand, &occupancy, &bridged),
        kv,
        (),
    ));
    futures_lite::future::block_on(matter)
}

/// The manual pairing code and the QR text for the test credentials.
pub fn setup_codes() -> Result<(String, String), Error> {
    let manual = TEST_BASIC_COMM.compute_pretty_pairing_code().to_string();
    let payload = QrPayload::new(
        DiscoveryCapabilities::IP,
        CommFlowType::Standard,
        TEST_BASIC_COMM.clone(),
        TEST_DEV_DET.vid,
        TEST_DEV_DET.pid,
        TEST_DEV_DET.serial_no,
        no_optional_data as NoOptionalData,
    );
    let mut buf = [0u8; 512];
    let (qr, _) = payload.as_str(&mut buf)?;
    Ok((manual, qr.to_string()))
}

fn write_setup_file(path: &Path) -> std::io::Result<()> {
    let (manual, qr) = setup_codes().map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    let body = format!(
        "# RuView Matter bridge setup (ADR-431 spike).\n\
         # CSA test credentials: for development only, not for other people's homes.\n\
         manual_code={manual}\n\
         qr={qr}\n"
    );
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(body.as_bytes())
}

const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            AGGREGATOR_EP,
            devices!(DEV_TYPE_AGGREGATOR),
            clusters!(desc::DescHandler::CLUSTER),
        ),
        Endpoint::new(
            ROOM_EP,
            devices!(DEV_TYPE_OCCUPANCY_SENSOR, DEV_TYPE_BRIDGED_NODE),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                RoomBridgedHandler::CLUSTER,
                OccupancyHandler::CLUSTER
            ),
        ),
    ],
};

fn data_model<'a>(
    matter: &'a Matter<'a>,
    mut rand: impl Rng + Copy,
    occupancy: &'a OccupancyHandler,
    bridged: &'a RoomBridgedHandler,
) -> impl DataModel + 'a {
    (
        NODE,
        EmptyHandler
            .chain(
                |e, _| e == ROOT_ENDPOINT_ID,
                Async(MatterStack::<0, EthNetwork>::root_handler(
                    matter,
                    &(),
                    rand,
                )),
            )
            .chain(
                |e, c| e == AGGREGATOR_EP && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == ROOM_EP && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == ROOM_EP && c == identify::CLUSTER.id,
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == ROOM_EP && c == RoomBridgedHandler::CLUSTER.id,
                Async(bdbi::HandlerAdaptor(bridged)),
            )
            .chain(
                |e, c| e == ROOM_EP && c == OccupancyHandler::CLUSTER.id,
                Async(occ::HandlerAdaptor(occupancy)),
            ),
    )
}

/// Occupancy Sensing server for the room.
///
/// The feature map says RF sensing. The spec maps a sensor with none of the
/// legacy modalities (PIR, ultrasonic, physical contact) to PIR in the legacy
/// `OccupancySensorType` attributes, for older controllers.
pub struct OccupancyHandler {
    dataver: Dataver,
    endpoint_id: EndptId,
    room: Arc<RoomOccupancy>,
}

impl OccupancyHandler {
    pub fn new(dataver: Dataver, endpoint_id: EndptId, room: Arc<RoomOccupancy>) -> Self {
        Self {
            dataver,
            endpoint_id,
            room,
        }
    }

    /// Unavailable reads as unoccupied; reachability says it is unknown.
    fn occupied(&self) -> bool {
        self.room.get() == Some(true)
    }
}

impl occ::ClusterHandler for OccupancyHandler {
    const CLUSTER: Cluster<'static> = occ::FULL_CLUSTER
        .with_features(occ::Feature::RF_SENSING.bits())
        .with_attrs(with!(required))
        .with_cmds(with!())
        .with_events(with!());

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        let mut last = self.occupied();
        loop {
            Timer::after(POLL).await;
            let now = self.occupied();
            if now != last {
                last = now;
                self.dataver.changed();
                ctx.notify_attr_changed(
                    self.endpoint_id,
                    Self::CLUSTER.id,
                    occ::AttributeId::Occupancy as _,
                );
                tracing::info!("Matter: room occupied -> {now}");
            }
        }
    }

    fn occupancy(&self, _ctx: impl ReadContext) -> Result<OccupancyBitmap, Error> {
        Ok(if self.occupied() {
            OccupancyBitmap::OCCUPIED
        } else {
            OccupancyBitmap::empty()
        })
    }

    fn occupancy_sensor_type(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<OccupancySensorTypeEnum, Error> {
        Ok(OccupancySensorTypeEnum::PIR)
    }

    fn occupancy_sensor_type_bitmap(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<OccupancySensorTypeBitmap, Error> {
        Ok(OccupancySensorTypeBitmap::PIR)
    }
}

/// Bridged Device Basic Information for the room. `Reachable` is false while
/// the room's occupancy is unavailable.
pub struct RoomBridgedHandler {
    dataver: Dataver,
    endpoint_id: EndptId,
    room: Arc<RoomOccupancy>,
    label: Mutex<String>,
}

impl RoomBridgedHandler {
    pub fn new(dataver: Dataver, endpoint_id: EndptId, room: Arc<RoomOccupancy>) -> Self {
        Self {
            dataver,
            endpoint_id,
            room,
            label: Mutex::new(ROOM_LABEL.to_string()),
        }
    }

    fn is_reachable(&self) -> bool {
        self.room.get().is_some()
    }
}

impl bdbi::ClusterHandler for RoomBridgedHandler {
    const CLUSTER: Cluster<'static> = bdbi::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required; bdbi::AttributeId::UniqueID | bdbi::AttributeId::NodeLabel))
        .with_cmds(with!());

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        let mut last = self.is_reachable();
        loop {
            Timer::after(POLL).await;
            let now = self.is_reachable();
            if now != last {
                last = now;
                self.dataver.changed();
                ctx.notify_attr_changed(
                    self.endpoint_id,
                    Self::CLUSTER.id,
                    bdbi::AttributeId::Reachable as _,
                );
                tracing::info!("Matter: room reachable -> {now}");
            }
        }
    }

    fn reachable(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.is_reachable())
    }

    fn unique_id<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        Ok(builder.set("ruview-room-1")?)
    }

    fn node_label<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let label = self
            .label
            .lock()
            .map_err(|_| Error::from(ErrorCode::Failure))?;
        Ok(builder.set(label.as_str())?)
    }

    fn set_node_label(&self, ctx: impl WriteContext, value: Utf8Str<'_>) -> Result<(), Error> {
        // The spec caps NodeLabel at 32 bytes.
        if value.len() > 32 {
            return Err(ErrorCode::ConstraintError.into());
        }
        let mut label = self
            .label
            .lock()
            .map_err(|_| Error::from(ErrorCode::Failure))?;
        *label = value.to_string();
        self.dataver.changed();
        ctx.notify_attr_changed(
            self.endpoint_id,
            Self::CLUSTER.id,
            bdbi::AttributeId::NodeLabel as _,
        );
        Ok(())
    }

    fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        // Not advertised (no BIS feature): refuse rather than panic.
        Err(ErrorCode::CommandNotFound.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matter::config::{TEST_PRODUCT_ID, TEST_VENDOR_ID};

    /// The handlers' poll loops sleep on embassy timers. This fails (panics
    /// or hangs) if the std timer driver or its queue isn't linked.
    #[test]
    fn embassy_timer_runs_on_this_platform() {
        let t0 = std::time::Instant::now();
        futures_lite::future::block_on(Timer::after(POLL));
        assert!(t0.elapsed() >= std::time::Duration::from_millis(100));
    }

    #[test]
    fn cli_test_identity_matches_the_bundled_attestation() {
        assert_eq!(TEST_DEV_DET.vid, TEST_VENDOR_ID);
        assert_eq!(TEST_DEV_DET.pid, TEST_PRODUCT_ID);
    }

    #[test]
    fn occupancy_cluster_advertises_rf_sensing_and_the_required_attributes() {
        let c = OccupancyHandler::CLUSTER;
        assert_eq!(c.id, 0x0406);
        assert_eq!(c.feature_map, occ::Feature::RF_SENSING.bits());
        let ids: Vec<u32> = c.attributes().map(|a| a.id).collect();
        for a in [
            occ::AttributeId::Occupancy,
            occ::AttributeId::OccupancySensorType,
            occ::AttributeId::OccupancySensorTypeBitmap,
        ] {
            assert!(ids.contains(&(a as u32)), "{a:?} missing from {ids:?}");
        }
        // No optional writable PIR timing attributes.
        assert!(!ids.contains(&(occ::AttributeId::PIROccupiedToUnoccupiedDelay as u32)));
    }

    #[test]
    fn bridged_cluster_has_reachable_unique_id_and_label() {
        let ids: Vec<u32> = RoomBridgedHandler::CLUSTER
            .attributes()
            .map(|a| a.id)
            .collect();
        for a in [
            bdbi::AttributeId::Reachable,
            bdbi::AttributeId::UniqueID,
            bdbi::AttributeId::NodeLabel,
        ] {
            assert!(ids.contains(&(a as u32)), "{a:?} missing from {ids:?}");
        }
    }

    #[test]
    fn node_has_root_aggregator_and_one_room() {
        let eps: Vec<EndptId> = NODE.endpoints.iter().map(|e| e.id).collect();
        assert_eq!(eps, vec![ROOT_ENDPOINT_ID, AGGREGATOR_EP, ROOM_EP]);
        let room = &NODE.endpoints[2];
        let types: Vec<u16> = room.device_types.iter().map(|d| d.dtype).collect();
        assert_eq!(
            types,
            vec![0x0107, 0x0013],
            "occupancy sensor + bridged node"
        );
    }

    #[test]
    fn setup_codes_are_the_test_codes() {
        let (manual, qr) = setup_codes().unwrap();
        // Passcode 20202021, discriminator 3840: the code every Matter
        // test device uses.
        assert_eq!(manual, "3497-011-2332");
        assert!(qr.starts_with("MT:"), "{qr}");
    }

    #[test]
    fn setup_file_holds_both_codes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.txt");
        write_setup_file(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("manual_code=3497-011-2332"), "{body}");
        assert!(body.contains("qr=MT:"), "{body}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
