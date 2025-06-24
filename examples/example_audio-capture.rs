use libspa::pod::Pod;
use libspa::utils::Direction;
use libspa_sys::*;
use pipewire::context::Context;
use pipewire::stream::{Stream, StreamFlags, StreamRef};
use pipewire::{keys, loop_::Signal, main_loop::MainLoop, properties::properties};
use std::io::Write;

struct StreamInfo {
    rate: u32,
    channels: u32,
    move_cursor: bool,
}
impl Default for StreamInfo {
    fn default() -> Self {
        StreamInfo {
            rate: 0,
            channels: 0,
            move_cursor: false,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mainloop = MainLoop::new(None)?;
    let context = Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let mainloop_clone = mainloop.clone();
    let _sigint = mainloop
        .loop_()
        .add_signal_local(Signal::SIGINT, move || mainloop_clone.quit());
    let mainloop_clone = mainloop.clone();
    let _sigterm = mainloop
        .loop_()
        .add_signal_local(Signal::SIGTERM, move || mainloop_clone.quit());

    let _microphone = unsafe {
        let props = properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_ROLE => "Music",
        };
        let microphone = Stream::new(&core, "audio-capture", props)?;
        microphone
            .add_local_listener_with_user_data(StreamInfo::default())
            .param_changed(on_microphone_format_changed)
            .process(on_microphone_frame)
            .register()?;
        let mut data = [0 as u8; 1024];
        let mut b: spa_pod_builder = std::mem::zeroed();
        b.data = data.as_mut_ptr() as *mut std::ffi::c_void;
        b.size = data.len() as u32;
        let mut info: spa_audio_info_raw = std::mem::zeroed();
        info.format = SPA_AUDIO_FORMAT_F32;
        let mut params: [&Pod; 1] = [Pod::from_raw(spa_format_audio_raw_build(
            &mut b,
            SPA_PARAM_EnumFormat,
            &mut info,
        ))];
        microphone.connect(
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )?;
        microphone
    };

    mainloop.run();

    Ok(())
}

fn on_microphone_format_changed(
    _stream: &StreamRef,
    info: &mut StreamInfo,
    id: u32,
    param: Option<&Pod>,
) {
    if param.is_none() || id != SPA_PARAM_Format {
        return;
    }
    let param: &spa_pod = unsafe { &*param.unwrap().as_raw_ptr() };

    let mut format: spa_audio_info;
    unsafe {
        format = std::mem::zeroed();
        if spa_format_parse(param, &mut format.media_type, &mut format.media_subtype) < 0 {
            return;
        }
    }
    if format.media_type != SPA_MEDIA_TYPE_audio || format.media_subtype != SPA_MEDIA_SUBTYPE_raw {
        return;
    }

    let mut format_raw: spa_audio_info_raw;
    unsafe {
        format_raw = std::mem::zeroed();
        spa_format_audio_raw_parse(param, &mut format_raw);
    }
    if info.move_cursor {
        print!("\x1b[{}A\x1b[J", info.channels + 2);
        info.move_cursor = false;
    }
    info.rate = format_raw.rate;
    info.channels = format_raw.channels;

    println!("capturing rate: {}, channels: {}", info.rate, info.channels);
}

fn on_microphone_frame(stream: &StreamRef, info: &mut StreamInfo) {
    use std::mem::*;

    match stream.dequeue_buffer() {
        None => println!("Out of buffers"),
        Some(mut buffer) => {
            let datas = buffer.datas_mut();
            assert_eq!(datas.len(), 1);

            let n_samples = datas[0].chunk().size() / size_of::<f32>() as u32;
            let samples = datas[0].data();
            if n_samples == 0 || samples.is_none() {
                return;
            }
            let (head, samples, tail) = unsafe { samples.unwrap().align_to::<f32>() };
            assert!(head.is_empty());
            assert!(tail.is_empty());

            if info.move_cursor {
                print!("\x1b[{}A\x1b[J", info.channels + 1);
            }
            println!(
                "captured {} samples ({} per channel) = {:.3}ms",
                n_samples,
                n_samples / info.channels,
                (n_samples / info.channels) as f32 / info.rate as f32 * 1000.,
            );
            for c in 0..info.channels {
                let mut max = 0f32;
                for n in (c as usize..n_samples as usize).step_by(info.channels as usize) {
                    max = samples[n].abs().max(max);
                }
                let peak = ((max * 30.) as usize).clamp(0, 39);
                let decibels = if max > 0. {
                    20f32 * max.log10()
                } else {
                    -f32::INFINITY
                };
                println!(
                    "channel {0}: |{1:>2$}{3:>4$}| peak: {5:3.1}dB {6}",
                    c,
                    "*",
                    peak + 1,
                    "",
                    40 - peak,
                    decibels,
                    max,
                );
            }

            info.move_cursor = true;
            std::io::stdout().flush().unwrap();
        }
    }
}
