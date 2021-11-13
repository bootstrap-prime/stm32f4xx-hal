//! Timers
//!
//! Pins can be used for PWM output in both push-pull mode (`Alternate`) and open-drain mode
//! (`AlternateOD`).

use cast::u16;
use cortex_m::peripheral::syst::SystClkSource;
use cortex_m::peripheral::{DCB, DWT, SYST};
use embedded_hal::timer::{Cancel, CountDown, Periodic};
use fugit::{MicrosDurationU32, TimerDurationU32};
use void::Void;

use crate::pac::RCC;

use crate::rcc::{self, Clocks};
use crate::time::Hertz;

#[cfg(feature = "rtic")]
#[cfg(not(feature = "stm32f410"))]
pub mod monotonic;

/// Timer wrapper
pub struct Timer<TIM> {
    pub(crate) tim: TIM,
    pub(crate) clk: Hertz,
}
/// Timer that waits given time
pub struct CountDownTimer<TIM, const FREQ: u32> {
    tim: TIM,
}

/// `CountDownTimer` with sampling of 1 MHz
pub type CountDownTimerUs<TIM> = CountDownTimer<TIM, 1_000_000>;

/// `CountDownTimer` with sampling of 1 kHz
///
/// NOTE: don't use this if your system frequency more than 65 MHz
pub type CountDownTimerMs<TIM> = CountDownTimer<TIM, 1_000>;

impl<TIM> Timer<TIM>
where
    TIM: Instance,
{
    /// Creates CountDownTimer with custom sampling
    pub fn count_down<const FREQ: u32>(self) -> CountDownTimer<TIM, FREQ> {
        let Self { tim, clk } = self;
        CountDownTimer::<TIM, FREQ>::new(tim, clk)
    }
    /// Creates CountDownTimer with sampling of 1 MHz
    pub fn count_down_us(self) -> CountDownTimerUs<TIM> {
        self.count_down::<1_000_000>()
    }

    /// Creates CountDownTimer with sampling of 1 kHz
    ///
    /// NOTE: don't use this if your system frequency more than 65 MHz
    pub fn count_down_ms(self) -> CountDownTimerMs<TIM> {
        self.count_down::<1_000>()
    }
}

impl<TIM, const FREQ: u32> Periodic for CountDownTimer<TIM, FREQ> {}

/// Interrupt events
pub enum Event {
    /// CountDownTimer timed out / count down ended
    TimeOut,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Error {
    /// CountDownTimer is disabled
    Disabled,
    WrongAutoReload,
}

impl Timer<SYST> {
    /// Initialize timer
    pub fn syst(mut syst: SYST, clocks: &Clocks) -> Self {
        syst.set_clock_source(SystClkSource::Core);
        Self {
            tim: syst,
            clk: clocks.sysclk(),
        }
    }

    /// Creates SysCountDownTimer
    pub fn count_down(self) -> SysCountDownTimer {
        let Self { tim, clk } = self;
        SysCountDownTimer { tim, clk }
    }

    pub fn release(self) -> SYST {
        self.tim
    }
}

pub struct SysCountDownTimer {
    tim: SYST,
    clk: Hertz,
}

impl SysCountDownTimer {
    /// Starts listening for an `event`
    pub fn listen(&mut self, event: Event) {
        match event {
            Event::TimeOut => self.tim.enable_interrupt(),
        }
    }

    /// Stops listening for an `event`
    pub fn unlisten(&mut self, event: Event) {
        match event {
            Event::TimeOut => self.tim.disable_interrupt(),
        }
    }
}

impl CountDown for SysCountDownTimer {
    type Time = MicrosDurationU32;

    fn start<T>(&mut self, timeout: T)
    where
        T: Into<Self::Time>,
    {
        let mul = self.clk.0 / 1_000_000;
        let rvr = timeout.into().ticks() * mul - 1;

        assert!(rvr < (1 << 24));

        self.tim.set_reload(rvr);
        self.tim.clear_current();
        self.tim.enable_counter();
    }

    fn wait(&mut self) -> nb::Result<(), Void> {
        if self.tim.has_wrapped() {
            Ok(())
        } else {
            Err(nb::Error::WouldBlock)
        }
    }
}

impl Cancel for SysCountDownTimer {
    type Error = Error;

    fn cancel(&mut self) -> Result<(), Self::Error> {
        if !self.tim.is_counter_enabled() {
            return Err(Self::Error::Disabled);
        }

        self.tim.disable_counter();
        Ok(())
    }
}

/// A monotonic non-decreasing timer
///
/// This uses the timer in the debug watch trace peripheral. This means, that if the
/// core is stopped, the timer does not count up. This may be relevant if you are using
/// cortex_m_semihosting::hprintln for debugging in which case the timer will be stopped
/// while printing
#[derive(Clone, Copy)]
pub struct MonoTimer {
    frequency: Hertz,
}

impl MonoTimer {
    /// Creates a new `Monotonic` timer
    pub fn new(mut dwt: DWT, mut dcb: DCB, clocks: &Clocks) -> Self {
        dcb.enable_trace();
        dwt.enable_cycle_counter();

        // now the CYCCNT counter can't be stopped or reset
        drop(dwt);

        MonoTimer {
            frequency: clocks.hclk(),
        }
    }

    /// Returns the frequency at which the monotonic timer is operating at
    pub fn frequency(self) -> Hertz {
        self.frequency
    }

    /// Returns an `Instant` corresponding to "now"
    pub fn now(self) -> Instant {
        Instant {
            now: DWT::get_cycle_count(),
        }
    }
}

/// A measurement of a monotonically non-decreasing clock
#[derive(Clone, Copy)]
pub struct Instant {
    now: u32,
}

impl Instant {
    /// Ticks elapsed since the `Instant` was created
    pub fn elapsed(self) -> u32 {
        DWT::get_cycle_count().wrapping_sub(self.now)
    }
}

mod sealed {
    pub trait General {
        type Width;
        fn enable_counter(&mut self);
        fn disable_counter(&mut self);
        fn is_counter_enabled(&self) -> bool;
        fn reset_counter(&mut self);
        fn set_prescaler(&mut self, psc: u16);
        fn set_auto_reload(&mut self, arr: u32) -> Result<(), super::Error>;
        fn trigger_update(&mut self);
        fn clear_update_interrupt_flag(&mut self);
        fn listen_update_interrupt(&mut self, b: bool);
        fn get_update_interrupt_flag(&self) -> bool;
    }
}
pub(crate) use sealed::General;

pub trait Instance: crate::Sealed + rcc::Enable + rcc::Reset + rcc::GetBusFreq + General {}

impl<TIM> Timer<TIM>
where
    TIM: Instance,
{
    /// Initialize timer
    pub fn new(tim: TIM, clocks: &Clocks) -> Self {
        unsafe {
            //NOTE(unsafe) this reference will only be used for atomic writes with no side effects
            let rcc = &(*RCC::ptr());
            // Enable and reset the timer peripheral
            TIM::enable(rcc);
            TIM::reset(rcc);
        }

        Self {
            clk: TIM::get_timer_frequency(clocks),
            tim,
        }
    }
}

macro_rules! hal {
    ($($TIM:ty: $bits:ty,)+) => {
        $(
            impl Instance for $TIM { }

            impl General for $TIM {
                type Width = $bits;

                #[inline(always)]
                fn enable_counter(&mut self) {
                    self.cr1.modify(|_, w| w.cen().set_bit());
                }
                #[inline(always)]
                fn disable_counter(&mut self) {
                    self.cr1.modify(|_, w| w.cen().clear_bit());
                }
                #[inline(always)]
                fn is_counter_enabled(&self) -> bool {
                    self.cr1.read().cen().is_enabled()
                }
                #[inline(always)]
                fn reset_counter(&mut self) {
                    self.cnt.reset();
                }
                #[inline(always)]
                fn set_prescaler(&mut self, psc: u16) {
                    self.psc.write(|w| w.psc().bits(psc) );
                }
                #[inline(always)]
                fn set_auto_reload(&mut self, arr: u32) -> Result<(), Error> {
                    if arr > 0 && arr <= <$bits>::MAX as u32 {
                        Ok(self.arr.write(|w| unsafe { w.bits(arr) }))
                    } else {
                        Err(Error::WrongAutoReload)
                    }
                }
                #[inline(always)]
                fn trigger_update(&mut self) {
                    self.cr1.modify(|_, w| w.urs().set_bit());
                    self.egr.write(|w| w.ug().set_bit());
                    self.cr1.modify(|_, w| w.urs().clear_bit());
                }
                #[inline(always)]
                fn clear_update_interrupt_flag(&mut self) {
                    self.sr.write(|w| w.uif().clear_bit());
                }
                #[inline(always)]
                fn listen_update_interrupt(&mut self, b: bool) {
                    self.dier.write(|w| w.uie().bit(b));
                }
                #[inline(always)]
                fn get_update_interrupt_flag(&self) -> bool {
                    self.sr.read().uif().bit_is_clear()
                }
            }
        )+
    }
}

impl<TIM, const FREQ: u32> CountDownTimer<TIM, FREQ>
where
    TIM: General,
{
    fn new(mut tim: TIM, clk: Hertz) -> Self {
        let psc = clk.0 / FREQ - 1;
        tim.set_prescaler(u16(psc).unwrap());
        Self { tim }
    }

    /// Starts listening for an `event`
    ///
    /// Note, you will also have to enable the TIM2 interrupt in the NVIC to start
    /// receiving events.
    pub fn listen(&mut self, event: Event) {
        match event {
            Event::TimeOut => {
                // Enable update event interrupt
                self.tim.listen_update_interrupt(true);
            }
        }
    }

    /// Clears interrupt associated with `event`.
    ///
    /// If the interrupt is not cleared, it will immediately retrigger after
    /// the ISR has finished.
    pub fn clear_interrupt(&mut self, event: Event) {
        match event {
            Event::TimeOut => {
                // Clear interrupt flag
                self.tim.clear_update_interrupt_flag();
            }
        }
    }

    /// Stops listening for an `event`
    pub fn unlisten(&mut self, event: Event) {
        match event {
            Event::TimeOut => {
                // Disable update event interrupt
                self.tim.listen_update_interrupt(false);
            }
        }
    }

    /// Releases the TIM peripheral
    pub fn release(mut self) -> TIM {
        // pause counter
        self.tim.disable_counter();
        self.tim
    }
}

impl<TIM, const FREQ: u32> CountDown for CountDownTimer<TIM, FREQ>
where
    TIM: General,
{
    type Time = TimerDurationU32<FREQ>;

    fn start<T>(&mut self, timeout: T)
    where
        T: Into<Self::Time>,
    {
        // pause
        self.tim.disable_counter();
        // reset counter
        self.tim.reset_counter();

        let arr = timeout.into().ticks() - 1;
        self.tim.set_auto_reload(arr).unwrap();

        // Trigger update event to load the registers
        self.tim.trigger_update();

        // start counter
        self.tim.enable_counter();
    }

    fn wait(&mut self) -> nb::Result<(), Void> {
        if self.tim.get_update_interrupt_flag() {
            Err(nb::Error::WouldBlock)
        } else {
            self.tim.clear_update_interrupt_flag();
            Ok(())
        }
    }
}

impl<TIM, const FREQ: u32> Cancel for CountDownTimer<TIM, FREQ>
where
    TIM: General,
{
    type Error = Error;

    fn cancel(&mut self) -> Result<(), Self::Error> {
        if !self.tim.is_counter_enabled() {
            return Err(Self::Error::Disabled);
        }

        // disable counter
        self.tim.disable_counter();
        Ok(())
    }
}

// All F4xx parts have these timers.
hal!(
    crate::pac::TIM1: u16,
    crate::pac::TIM5: u32,
    crate::pac::TIM9: u16,
    crate::pac::TIM11: u16,
);

// All parts except for F410 add these timers.
#[cfg(any(
    feature = "stm32f401",
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f411",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
hal!(
    crate::pac::TIM2: u32,
    crate::pac::TIM3: u16,
    crate::pac::TIM4: u16,
    crate::pac::TIM10: u16,
);

// All parts except F401 and F411.
#[cfg(any(
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f410",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
hal!(crate::pac::TIM6: u16,);

// All parts except F401, F410, F411.
#[cfg(any(
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
hal!(
    crate::pac::TIM7: u16,
    crate::pac::TIM8: u16,
    crate::pac::TIM12: u16,
    crate::pac::TIM13: u16,
    crate::pac::TIM14: u16,
);

#[allow(unused)]
#[cfg(feature = "gpiod")]
use crate::gpio::gpiod::*;
#[allow(unused)]
#[cfg(feature = "gpioe")]
use crate::gpio::gpioe::*;
#[allow(unused)]
#[cfg(feature = "gpiof")]
use crate::gpio::gpiof::*;
#[allow(unused)]
#[cfg(feature = "gpioi")]
use crate::gpio::gpioi::*;
use crate::gpio::{gpioa::*, gpiob::*, Alternate, AlternateOD};
#[allow(unused)]
use crate::gpio::{gpioc::*, gpioh::*};

// Output channels marker traits
pub trait PinC1<TIM> {}
pub trait PinC2<TIM> {}
pub trait PinC3<TIM> {}
pub trait PinC4<TIM> {}

macro_rules! channel_impl {
    ( $( $TIM:ident, $PINC:ident, $PINX:ident, $AF:literal; )+ ) => {
        $(
            impl $PINC<crate::pac::$TIM> for $PINX<Alternate<$AF>> {}
            impl $PINC<crate::pac::$TIM> for $PINX<AlternateOD<$AF>> {}
        )+
    };
}

// The approach to PWM channel implementation is to group parts with
// common pins, starting with groupings of the largest number of parts
// and moving to smaller and smaller groupings.  Last, we have individual
// parts to cover exceptions.

// All parts have these PWM pins.
channel_impl!(
    TIM1, PinC1, PA8, 1;
    TIM1, PinC2, PA9, 1;
    TIM1, PinC3, PA10, 1;
    TIM1, PinC4, PA11, 1;

    TIM5, PinC1, PA0, 2;
    TIM5, PinC2, PA1, 2;
    TIM5, PinC3, PA2, 2;
    TIM5, PinC4, PA3, 2;

    TIM9, PinC1, PA2, 3;
    TIM9, PinC2, PA3, 3;

    TIM11, PinC1, PB9, 3;
);

// All parts except F410.
#[cfg(any(
    feature = "stm32f401",
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f411",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
channel_impl!(
    TIM1, PinC1, PE9, 1;
    TIM1, PinC2, PE11, 1;
    TIM1, PinC3, PE13, 1;
    TIM1, PinC4, PE14, 1;

    TIM2, PinC1, PA0, 1;
    TIM2, PinC2, PA1, 1;
    TIM2, PinC3, PA2, 1;
    TIM2, PinC4, PA3, 1;

    TIM2, PinC2, PB3, 1;
    TIM2, PinC3, PB10, 1;
    TIM2, PinC4, PB11, 1;

    TIM2, PinC1, PA5, 1;
    TIM2, PinC1, PA15, 1;

    TIM3, PinC1, PA6, 2;
    TIM3, PinC2, PA7, 2;
    TIM3, PinC3, PB0, 2;
    TIM3, PinC4, PB1, 2;

    TIM3, PinC1, PB4, 2;
    TIM3, PinC2, PB5, 2;

    TIM3, PinC1, PC6, 2;
    TIM3, PinC2, PC7, 2;
    TIM3, PinC3, PC8, 2;
    TIM3, PinC4, PC9, 2;

    TIM4, PinC1, PB6, 2;
    TIM4, PinC2, PB7, 2;
    TIM4, PinC3, PB8, 2;
    TIM4, PinC4, PB9, 2;

    TIM4, PinC1, PD12, 2;
    TIM4, PinC2, PD13, 2;
    TIM4, PinC3, PD14, 2;
    TIM4, PinC4, PD15, 2;

    TIM10, PinC1, PB8, 3;
);

// All parts except F401 and F410.
#[cfg(any(
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f411",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
channel_impl!(
    TIM9, PinC1, PE5, 3;
    TIM9, PinC2, PE6, 3;
);

// All parts except F401, F410, and F411.
#[cfg(any(
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f412",
    feature = "stm32f413",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f423",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f446",
    feature = "stm32f469",
    feature = "stm32f479"
))]
channel_impl!(
    TIM8, PinC1, PC6, 3;
    TIM8, PinC2, PC7, 3;
    TIM8, PinC3, PC8, 3;
    TIM8, PinC4, PC9, 3;

    TIM10, PinC1, PF6, 3;

    TIM11, PinC1, PF7, 3;

    TIM12, PinC1, PB14, 9;
    TIM12, PinC2, PB15, 9;

    TIM13, PinC1, PA6, 9;
    TIM13, PinC1, PF8, 9;  // Not a mistake: TIM13 has only one channel.

    TIM14, PinC1, PA7, 9;
    TIM14, PinC1, PF9, 9;  // Not a mistake: TIM14 has only one channel.
);

// STM's "advanced and foundation" lines except F446.
#[cfg(any(
    feature = "stm32f405",
    feature = "stm32f407",
    feature = "stm32f415",
    feature = "stm32f417",
    feature = "stm32f427",
    feature = "stm32f429",
    feature = "stm32f437",
    feature = "stm32f439",
    feature = "stm32f469",
    feature = "stm32f479"
))]
channel_impl!(
    TIM5, PinC1, PH10, 2;
    TIM5, PinC2, PH11, 2;
    TIM5, PinC3, PH12, 2;
    TIM5, PinC4, PI0, 2;

    TIM8, PinC1, PI5, 3;
    TIM8, PinC2, PI6, 3;
    TIM8, PinC3, PI7, 3;
    TIM8, PinC4, PI2, 3;

    TIM12, PinC1, PH6, 9;
    TIM12, PinC2, PH9, 9;
);

#[cfg(any(feature = "stm32f412", feature = "stm32f413", feature = "stm32f423"))]
channel_impl!(
    TIM5, PinC1, PF3, 2;
    TIM5, PinC2, PF4, 2;
    TIM5, PinC3, PF5, 2;
    TIM5, PinC4, PF10, 2;
);

#[cfg(feature = "stm32f410")]
channel_impl!(
    TIM5, PinC1, PB12, 2;
    TIM5, PinC2, PC10, 2;
    TIM5, PinC3, PC11, 2;
    TIM5, PinC4, PB11, 2;

    TIM9, PinC1, PC4, 3;
    TIM9, PinC2, PC5, 3;

    TIM11, PinC1, PC13, 3;
);

#[cfg(feature = "stm32f446")]
channel_impl!(
    TIM2, PinC1, PB8, 1;
    TIM2, PinC2, PB9, 1;

    TIM2, PinC4, PB2, 1;
);
