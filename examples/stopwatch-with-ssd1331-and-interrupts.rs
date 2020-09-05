//! A simple stopwatch app running on an SSD1331 display
//!
//! This example requires the `rt` feature to be enabled. For example, to run on an STM32F4Discovery
//! dev board, run the following:
//!
//! ```bash
//! cargo run --features stm32f407,rt --release --example stopwatch-with-ssd1331-and-interrupts
//! ```
//!
//! Note that `--release` is required to fix link errors for smaller devices.
//!
//! Press the User button on an STM32F4Discovery board to start/stop the timer. Pressing the Reset
//! button will reset the stopwatch to zero.
//!
//! Video of this example running: https://imgur.com/a/lQTQFLy

#![no_std]
#![no_main]

extern crate panic_semihosting; // logs messages to the host stderr; requires a debugger
extern crate stm32f4xx_hal as hal;

use crate::hal::{
    gpio::{gpioa::PA0, Edge, ExtiPin, Input, PullDown},
    spi::{Spi, Mode, Phase, Polarity},
    interrupt,
    prelude::*,
    rcc::{Clocks, Rcc},
    stm32,
    timer::{Event, Timer},
};
use arrayvec::ArrayString;
use core::cell::{Cell, RefCell};
use core::fmt;
use core::ops::DerefMut;
use cortex_m::interrupt::{free, CriticalSection, Mutex};
use cortex_m_rt::{entry, exception, ExceptionFrame};
use embedded_graphics::{
    fonts::{Font12x16, Font6x12, Text},
    pixelcolor::Rgb565,
    prelude::*,
    style::TextStyleBuilder,
};
use ssd1331::{DisplayRotation::Rotate0, Ssd1331};

// Set up global state. It's all mutexed up for concurrency safety.
static ELAPSED_MS: Mutex<Cell<u32>> = Mutex::new(Cell::new(0u32));
static TIMER_TIM2: Mutex<RefCell<Option<Timer<stm32::TIM2>>>> = Mutex::new(RefCell::new(None));
static STATE: Mutex<Cell<StopwatchState>> = Mutex::new(Cell::new(StopwatchState::Ready));
static BUTTON: Mutex<RefCell<Option<PA0<Input<PullDown>>>>> = Mutex::new(RefCell::new(None));

#[derive(Clone, Copy)]
enum StopwatchState {
    Ready,
    Running,
    Stopped,
}

#[entry]
fn main() -> ! {
    if let (Some(mut dp), Some(cp)) = (stm32::Peripherals::take(), cortex_m::Peripherals::take()) {
        dp.RCC.apb2enr.write(|w| w.syscfgen().enabled());

        let rcc = dp.RCC.constrain();
        let clocks = setup_clocks(rcc);

        //spi2
        //sck  - pb13
        //miso - pb14 (not wired)
        //mosi(sda) - pb15
        //cs - pb11
        //dc - pb1
        //rst - pb0
        //vcc - 3v
        //gnd - gnd

        let gpiob = dp.GPIOB.split();
        let sck = gpiob.pb13.into_alternate_af5().internal_pull_up(false);
        let miso = gpiob.pb14.into_alternate_af5().internal_pull_up(false);
        let mosi = gpiob.pb15.into_alternate_af5().internal_pull_up(false);
        let mut rst = gpiob.pb0.into_push_pull_output();
        let dc = gpiob.pb1.into_push_pull_output();

        let spi = Spi::spi2(
            dp.SPI2,
            (sck, miso, mosi),
            Mode {
                polarity: Polarity::IdleLow,
                phase: Phase::CaptureOnFirstTransition,
            },
            8.mhz().into(),
            clocks,
        );

        // Create a button input with an interrupt
        let gpioa = dp.GPIOA.split();
        let mut board_btn = gpioa.pa0.into_pull_down_input();
        board_btn.make_interrupt_source(&mut dp.SYSCFG);
        board_btn.enable_interrupt(&mut dp.EXTI);
        board_btn.trigger_on_edge(&mut dp.EXTI, Edge::FALLING);

        let mut ss = gpiob.pb11.into_push_pull_output();
        let mut delay = hal::delay::Delay::new(cp.SYST, clocks);

        ss.set_high().unwrap();
        delay.delay_ms(100_u32);
        ss.set_low().unwrap();

        // Set up the display
        let mut disp = Ssd1331::new(spi, dc, Rotate0);
        disp.reset(&mut rst, &mut delay).unwrap();
        disp.init().unwrap();
        disp.flush().unwrap();

        // Create a 1ms periodic interrupt from TIM2
        let mut timer = Timer::tim2(dp.TIM2, 1.khz(), clocks);
        timer.listen(Event::TimeOut);

        free(|cs| {
            TIMER_TIM2.borrow(cs).replace(Some(timer));
            BUTTON.borrow(cs).replace(Some(board_btn));
        });

        // Enable interrupts
        stm32::NVIC::unpend(hal::stm32::Interrupt::TIM2);
        stm32::NVIC::unpend(hal::stm32::Interrupt::EXTI0);
        unsafe {
            stm32::NVIC::unmask(hal::stm32::Interrupt::EXTI0);
        };

        loop {
            let elapsed = free(|cs| ELAPSED_MS.borrow(cs).get());

            let mut format_buf = ArrayString::<[u8; 10]>::new();
            format_elapsed(&mut format_buf, elapsed);

            disp.clear();

            let state = free(|cs| STATE.borrow(cs).get());
            let state_msg = match state {
                StopwatchState::Ready => "Ready",
                StopwatchState::Running => "",
                StopwatchState::Stopped => "Stopped",
            };

            let text_style = TextStyleBuilder::new(Font6x12)
                .text_color(Rgb565::WHITE)
                .build();

            let text_style_format_buf = TextStyleBuilder::new(Font12x16)
                .text_color(Rgb565::WHITE)
                .build();

            Text::new(state_msg, Point::new(0, 0))
                .into_styled(text_style)
                .draw(&mut disp)
                .unwrap();

            Text::new(format_buf.as_str(), Point::new(0, 14))
                .into_styled(text_style_format_buf)
                .draw(&mut disp)
                .unwrap();

            disp.flush().unwrap();

            delay.delay_ms(100u32);
        }
    }

    loop {}
}

#[interrupt]
fn TIM2() {
    free(|cs| {
        if let Some(ref mut tim2) = TIMER_TIM2.borrow(cs).borrow_mut().deref_mut() {
            tim2.clear_interrupt(Event::TimeOut);
        }

        let cell = ELAPSED_MS.borrow(cs);
        let val = cell.get();
        cell.replace(val + 1);
    });
}

#[interrupt]
fn EXTI0() {
    free(|cs| {
        let mut btn_ref = BUTTON.borrow(cs).borrow_mut();
        if let Some(ref mut btn) = btn_ref.deref_mut() {
            // We cheat and don't bother checking _which_ exact interrupt line fired - there's only
            // ever going to be one in this example.
            btn.clear_interrupt_pending_bit();

            let state = STATE.borrow(cs).get();
            // Run the state machine in an ISR - probably not something you want to do in most
            // cases but this one only starts and stops TIM2 interrupts
            match state {
                StopwatchState::Ready => {
                    stopwatch_start(cs);
                    STATE.borrow(cs).replace(StopwatchState::Running);
                }
                StopwatchState::Running => {
                    stopwatch_stop(cs);
                    STATE.borrow(cs).replace(StopwatchState::Stopped);
                }
                StopwatchState::Stopped => {}
            }
        }
    });
}

fn setup_clocks(rcc: Rcc) -> Clocks {
    return rcc
        .cfgr
        .hclk(168.mhz())
        .sysclk(168.mhz())
        .pclk1(42.mhz())
        .pclk2(84.mhz())
        .freeze();
}

fn stopwatch_start<'cs>(cs: &'cs CriticalSection) {
    ELAPSED_MS.borrow(cs).replace(0);
    unsafe {
        stm32::NVIC::unmask(hal::stm32::Interrupt::TIM2);
    }
}

fn stopwatch_stop<'cs>(_cs: &'cs CriticalSection) {
    stm32::NVIC::mask(hal::stm32::Interrupt::TIM2);
}

// Formatting requires the arrayvec crate
fn format_elapsed(buf: &mut ArrayString<[u8; 10]>, elapsed: u32) {
    let minutes = elapsed_to_m(elapsed);
    let seconds = elapsed_to_s(elapsed);
    let millis = elapsed_to_ms(elapsed);
    fmt::write(
        buf,
        format_args!("{}:{:02}.{:03}", minutes, seconds, millis),
    )
    .unwrap();
}

fn elapsed_to_ms(elapsed: u32) -> u32 {
    return elapsed % 1000;
}

fn elapsed_to_s(elapsed: u32) -> u32 {
    return (elapsed - elapsed_to_ms(elapsed)) % 60000 / 1000;
}

fn elapsed_to_m(elapsed: u32) -> u32 {
    return elapsed / 60000;
}

#[exception]
fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("{:#?}", ef);
}
