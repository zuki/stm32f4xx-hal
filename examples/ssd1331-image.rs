//! Draw Ferris the Rust mascot on an SSD1331 display
//!
//! This example requires the `rt` feature to be enabled. For example, to run on an STM32F4Discovery
//! board, run the following:
//!
//! ```bash
//! cargo run --features stm32f407,rt --release --example ssd1331-image
//! ```
//!
//! Note that `--release` is required to fix link errors for smaller devices.

#![no_std]
#![no_main]

extern crate cortex_m;
extern crate cortex_m_rt as rt;
extern crate panic_semihosting;
extern crate stm32f4xx_hal as hal;

use cortex_m_rt::ExceptionFrame;
use cortex_m_rt::{entry, exception};
use embedded_graphics::{image::Image, image::ImageRaw, pixelcolor::Rgb565, prelude::*};
use ssd1331::{DisplayRotation, Ssd1331};

use crate::hal::spi::{Spi, Mode, Phase, Polarity};
use crate::hal::{prelude::*, stm32};

#[entry]
fn main() -> ! {
    if let (Some(dp), Some(cp)) = (
        stm32::Peripherals::take(),
        cortex_m::peripheral::Peripherals::take(),
    ) {
        // Set up the system clock. We want to run at 48MHz for this one.
        let rcc = dp.RCC.constrain();
        let clocks = rcc.cfgr.sysclk(48.mhz()).freeze();
        let mut delay = hal::delay::Delay::new(cp.SYST, clocks);

        let gpiob = dp.GPIOB.split();
        let sck = gpiob.pb13.into_alternate_af5().internal_pull_up(false);
        let miso = gpiob.pb14.into_alternate_af5().internal_pull_up(false);
        let mosi = gpiob.pb15.into_alternate_af5().internal_pull_up(false);
        let mut rst = gpiob.pb0.into_push_pull_output();
        let dc = gpiob.pb1.into_push_pull_output();
        let mut chip_select = gpiob.pb11.into_push_pull_output();
        chip_select.set_low().ok();

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

        // There's a button on PA0.
        let gpioa = dp.GPIOA.split();
        let btn = gpioa.pa0.into_pull_down_input();

        // Set up the display
        let mut disp = Ssd1331::new(spi, dc, DisplayRotation::Rotate0);
        disp.reset(&mut rst, &mut delay).unwrap();
        disp.init().unwrap();
        disp.flush().unwrap();

        // ssd1306-image.dataの画像は1bitの白黒画像で、RGB565モードで読み込もうとすると
        // ImageRaw::new()内の`assert_eq!(data.len(), height as usize * ret.bytes_per_row());`が
        // エラーとなる。そのため、ssd1331クレートのexampleにある`ferris.raw`を使うことにした。
        let raw_image: ImageRaw<Rgb565> = ImageRaw::new(include_bytes!("./ferris.raw"), 86, 64);
        let image: Image<_, Rgb565> = Image::new(&raw_image, Point::new((96 - 86) / 2, 0));
        image.draw(&mut disp).unwrap();
        disp.flush().unwrap();

        // Set up state for the loop
        let mut orientation = DisplayRotation::Rotate0;
        let mut was_pressed = btn.is_low().unwrap();

        // This runs continuously, as fast as possible
        loop {
            // Check if the button has just been pressed.
            // Remember, active low.
            let is_pressed = btn.is_low().unwrap();
            if !was_pressed && is_pressed {
                // Since the button was pressed, flip the screen upside down
                orientation = get_next_rotation(orientation);
                disp.set_rotation(orientation).unwrap();
                // Now that we've flipped the screen, store the fact that the button is pressed.
                was_pressed = true;
            } else if !is_pressed {
                // If the button is released, confirm this so that next time it's pressed we'll
                // know it's time to flip the screen.
                was_pressed = false;
            }
        }
    }

    loop {}
}

/// Helper function - what rotation flips the screen upside down from
/// the rotation we're in now?
fn get_next_rotation(rotation: DisplayRotation) -> DisplayRotation {
    return match rotation {
        DisplayRotation::Rotate0 => DisplayRotation::Rotate180,
        DisplayRotation::Rotate180 => DisplayRotation::Rotate0,

        // Default branch - if for some reason we end up in one of the portrait modes,
        // reset to 0 degrees landscape. On most SSD1306 displays, this means down is towards
        // the flat flex coming out of the display (and up is towards the breakout board pins).
        _ => DisplayRotation::Rotate0,
    };
}

#[exception]
fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("{:#?}", ef);
}
