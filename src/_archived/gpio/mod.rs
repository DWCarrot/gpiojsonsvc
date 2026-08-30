pub mod mock;
pub mod pinspec;

use crate::protocol::request::BiasMode;
use crate::protocol::request::DriveMode;
use crate::protocol::request::EdgeMode;
use crate::protocol::response::EventType;


/// input config
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GPIOPinConfig {
    Input { bias: BiasMode },
    Output { drive: DriveMode },
    Trigger { edge: EdgeMode },
}


/// Pin level
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLevel {
    Low = 0,
    High = 1,
}

impl From<u8> for PinLevel {
    fn from(value: u8) -> Self {
        if value == 0 {
            Self::Low
        } else {
            Self::High
        }
    }
}

impl From<PinLevel> for u8 {
    fn from(value: PinLevel) -> Self {
        value as u8
    }
}

impl PinLevel {

    pub unsafe fn from_u8_unchecked(value: u8) -> Self {
        std::mem::transmute(value)
    }
}


/// GPIO properties related
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GPIOProperties {
    Input(GPIOInputProperties),
    Output(GPIOOutputProperties),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GPIOInputProperties {
    bias: PropertyBiasMode,
    event: PropertyEventMode,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyBiasMode {
    Disabled = 0,
    PullUp = 1,
    PullDown = 2,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyEventMode {
    None = 0,
    Rising = 1,
    Falling = 2,
    Both = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GPIOOutputProperties {
    drive: PropertyDriveMode,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyDriveMode {
    PushPull = 0,
    OpenDrain = 1,
    OpenSource = 2,
}

impl From<BiasMode> for PropertyBiasMode {
    fn from(bias: BiasMode) -> Self {
        match bias {
            BiasMode::PullUp => Self::PullUp,
            BiasMode::PullDown => Self::PullDown,
            BiasMode::AsIs | BiasMode::Disabled => Self::Disabled,
        }
    }
}

impl From<DriveMode> for PropertyDriveMode {
    fn from(drive: DriveMode) -> Self {
        match drive {
            DriveMode::PushPull => Self::PushPull,
            DriveMode::OpenDrain => Self::OpenDrain,
            DriveMode::OpenSource => Self::OpenSource,
        }
    }
}

impl From<EdgeMode> for PropertyEventMode {
    fn from(edge: EdgeMode) -> Self {
        match edge {
            EdgeMode::Rising => Self::Rising,
            EdgeMode::Falling => Self::Falling,
            EdgeMode::Both => Self::Both,
        }
    }
}

impl GPIOProperties {
    pub(crate) fn input(bias: PropertyBiasMode, event: PropertyEventMode) -> Self {
        Self::Input(GPIOInputProperties { bias, event })
    }

    pub(crate) fn output(drive: PropertyDriveMode) -> Self {
        Self::Output(GPIOOutputProperties { drive })
    }

    pub fn from_config(config: &GPIOPinConfig) -> Self {
        match config {
            GPIOPinConfig::Input { bias } => {
                Self::input(PropertyBiasMode::from(*bias), PropertyEventMode::None)
            }
            GPIOPinConfig::Trigger { edge } => {
                Self::input(PropertyBiasMode::Disabled, PropertyEventMode::from(*edge))
            }
            GPIOPinConfig::Output { drive } => Self::output(PropertyDriveMode::from(*drive)),
        }
    }
}


pub trait GPIOClusterError: std::error::Error + Send + Sync {
    
    fn is_closed(&self) -> bool;

    fn is_invalid_index(&self) -> bool;

    fn is_unmatched_value(&self) -> bool;
}

/// traits
pub trait GPIOCluster: Send + Sync {

    type Error: GPIOClusterError;

    /// get the number of pins in the clustert
    fn pin_count(&self) -> usize;

    /// get the name of the pin at the given index
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check if the index is out of bounds.
    unsafe fn pin_name_unchecked(&self, index: usize) -> &str;

    /// get the name of the pin at the given index
    fn pin_name(&self, index: usize) -> Option<&str>;

    /// get the properties of the pin at the given index
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check if the index is out of bounds.
    unsafe fn pin_props_unchecked(&self, index: usize) -> GPIOProperties;

    /// get the properties of the pin at the given index
    fn pin_props(&self, index: usize) -> Option<GPIOProperties>;

    /// read the values of the pins at the given indices asynchronously
    ///
    /// # Arguments
    ///
    /// * `indices`: an iterator of indices of the pins to read
    /// 
    /// # Returns
    ///
    /// Returns an iterator of the values of the pins at the given indices.
    async fn read(&self, indices: impl Iterator<Item = usize>) -> Result<impl Iterator<Item = PinLevel>, Self::Error>;

    /// write the values of the pins at the given indices asynchronously
    ///
    /// # Arguments
    ///
    /// * `indices`: an iterator of indices of the pins to write
    /// * `values`: an iterator of values to write
    /// note: the length of the `indices` and `values` must be the same.
    async fn write(&self, indices: impl Iterator<Item = usize>, values: impl Iterator<Item = PinLevel>) -> Result<(), Self::Error>;


    /// wait for an event asynchronously
    /// # Returns
    ///
    /// Returns the index of the pin that triggered the event and the type of the event.
    /// if no pin is in event mode, this function will never resolve until the cluster is closed.
    async fn wait(&self) -> Result<(usize, EventType), Self::Error>;

    /// close the cluster asynchronously
    /// After this call, the cluster will be closed and can not be used anymore.
    /// with `read`, `write` and `wait` will return an error.
    /// If this is not called, the cluster will be closed automatically when dropped, but in none-async context.
    async fn close(&self);
}

pub trait GPIOBackend {
 
    type Error: GPIOClusterError;
    type Cluster: GPIOCluster<Error = Self::Error>;

    /// create a new cluster
    ///
    /// # Arguments
    ///
    /// * `configs`: an iterator of pairs of pin names and configurations
    ///
    /// # Returns
    ///
    /// Returns the cluster
    async fn cluster(&self, configs: impl Iterator<Item = (&str, GPIOPinConfig)>) -> Result<Self::Cluster, Self::Error>;
}
