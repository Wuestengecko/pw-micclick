use anyhow::Result;
use clap::Parser;
use ears::{AudioController, Sound};
use gtk::prelude::*;
use libappindicator::{AppIndicator, AppIndicatorStatus};
use libspa::pod::Pod;
use libspa::utils::Direction;
use libspa_sys::*;
use pipewire::context::Context;
use pipewire::core::Core;
use pipewire::keys;
use pipewire::loop_::Signal;
use pipewire::main_loop::MainLoop;
use pipewire::properties::properties;
use pipewire::stream::{Stream, StreamFlags, StreamListener, StreamRef};
use std::mem::{size_of, zeroed};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(clap::Parser)]
struct Args {
    #[arg(long, default_value = "-60")]
    /// The input threshold volume in dB.
    threshold: f32,

    #[arg(long, value_parser = |v: &str| -> Result<Duration> { Ok(Duration::from_millis(v.parse()?)) }, default_value="750")]
    /// Hold the "on" state this many milliseconds after microphone input stopped.
    hold_time: Duration,

    #[arg(long)]
    /// Sound to play when microphone input is detected.
    on_sound: Option<String>,

    #[arg(long)]
    /// Sound to play when no microphone input is detected anymore.
    off_sound: Option<String>,
}

#[derive(Debug, Copy, Clone)]
enum ThresholdCrossed {
    Rising,
    Falling,
}

struct CaptureState {
    queues: Vec<mpsc::Sender<ThresholdCrossed>>,
    threshold: f32,
    hold_time: Duration,
    falloff: Instant,
    is_on: bool,

    on_sound: Option<Sound>,
    off_sound: Option<Sound>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (tray_sender, tray_receiver) = mpsc::channel();
    let _tray_thread = thread::spawn(move || tray_thread_main(tray_receiver));

    let mainloop = MainLoop::new(None)?;
    let context = Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let _sigint = mainloop.loop_().add_signal_local(Signal::SIGINT, {
        let mainloop = mainloop.clone();
        move || mainloop.quit()
    });
    let _sigterm = mainloop.loop_().add_signal_local(Signal::SIGTERM, {
        let mainloop = mainloop.clone();
        move || mainloop.quit()
    });

    let senders = vec![tray_sender];
    let _capture = create_capture(&core, senders, &args)?;

    mainloop.run();

    Ok(())
}

#[derive(Debug, Error)]
enum InitError {
    #[error("{0}")]
    CannotLoadSound(String),
}

fn create_capture(
    core: &Core,
    senders: Vec<mpsc::Sender<ThresholdCrossed>>,
    args: &Args,
) -> Result<(Stream, StreamListener<CaptureState>)> {
    let state = CaptureState {
        queues: senders,
        threshold: 10f32.powf(args.threshold / 20.),
        hold_time: args.hold_time,
        falloff: Instant::now(),
        is_on: false,
        on_sound: match args.on_sound.as_deref() {
            Some(path) => Some(Sound::new(path).map_err(|e| InitError::CannotLoadSound(e))?),
            None => None,
        },
        off_sound: match args.off_sound.as_deref() {
            Some(path) => Some(Sound::new(path).map_err(|e| InitError::CannotLoadSound(e))?),
            None => None,
        },
    };

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Accessibility",
        *keys::NODE_PASSIVE => "in",
    };
    let stream = Stream::new(&core, "micclick-capture", props)?;
    let listener = stream
        .add_local_listener_with_user_data(state)
        .process(on_microphone_frame)
        .register()?;
    let mut data = [0 as u8; 1024];
    let mut b: spa_pod_builder = unsafe { zeroed() };
    b.data = data.as_mut_ptr() as *mut std::ffi::c_void;
    b.size = data.len() as u32;
    let mut info: spa_audio_info_raw = unsafe { zeroed() };
    info.format = SPA_AUDIO_FORMAT_F32;
    let mut params: [&Pod; 1] = unsafe {
        [Pod::from_raw(spa_format_audio_raw_build(
            &mut b,
            SPA_PARAM_EnumFormat,
            &mut info,
        ))]
    };
    stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    Ok((stream, listener))
}

fn on_microphone_frame(stream: &StreamRef, state: &mut CaptureState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        println!("error: capture stream is out of buffers");
        return;
    };
    let datas = buffer.datas_mut();
    assert_eq!(datas.len(), 1, "expected exactly one data buffer");

    let n_samples = datas[0].chunk().size() / size_of::<f32>() as u32;
    if n_samples == 0 {
        return;
    }
    let Some(samples) = datas[0].data() else {
        return;
    };
    let (head, samples, tail) = unsafe { samples.align_to::<f32>() };
    assert!(head.is_empty(), "misaligned data buffer");
    assert!(tail.is_empty(), "misaligned data buffer");

    let mut max = 0f32;
    for n in 0..n_samples {
        max = samples[n as usize].abs().max(max);
    }
    let max = max;

    let now = Instant::now();
    if max > state.threshold {
        state.falloff = now + state.hold_time;
    }

    let event: ThresholdCrossed;
    match (state.is_on, now <= state.falloff) {
        (false, true) => {
            state.is_on = true;
            event = ThresholdCrossed::Rising;
            if let Some(ref mut sound) = state.on_sound {
                sound.play();
            }
        }
        (true, false) => {
            state.is_on = false;
            event = ThresholdCrossed::Falling;
            if let Some(ref mut sound) = state.off_sound {
                sound.play();
            }
        }
        _ => return,
    }
    for q in state.queues.iter() {
        q.send(event).expect("cannot send: channel broken");
    }
}

static mut INDICATOR_MENU: *mut gtk::Menu = std::ptr::null_mut();
static mut INDICATOR: *mut AppIndicator = std::ptr::null_mut();
static INDICATOR_INIT: std::sync::Once = std::sync::Once::new();

fn tray_thread_main(eventreceiver: mpsc::Receiver<ThresholdCrossed>) {
    gtk::init().expect("gtk::init() failed");

    gtk::glib::source::idle_add(move || {
        INDICATOR_INIT.call_once(|| unsafe {
            INDICATOR = Box::into_raw(Box::new(AppIndicator::new("pw-micclick", "")));
            (*INDICATOR).set_status(AppIndicatorStatus::Active);
            (*INDICATOR).set_icon_full("microphone-sensitivity-muted-symbolic", "icon");

            INDICATOR_MENU = Box::into_raw(Box::new(gtk::Menu::new()));
            (*INDICATOR).set_menu(&mut *INDICATOR_MENU);
            (*INDICATOR_MENU).show_all();
        });

        let indicator = unsafe { &mut *INDICATOR };
        match eventreceiver.try_recv() {
            Ok(ThresholdCrossed::Rising) => {
                indicator.set_icon_full("microphone-sensitivity-high-symbolic", "icon");
                gtk::glib::ControlFlow::Continue
            }
            Ok(ThresholdCrossed::Falling) => {
                indicator.set_icon_full("microphone-sensitivity-muted-symbolic", "icon");
                gtk::glib::ControlFlow::Continue
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                gtk::main_quit();
                gtk::glib::ControlFlow::Break
            }
        }
    });
    gtk::main();
}
