//! Edit › Copy Special › Audio as MIDI: pitch-track the selected audio into MIDI notes on the
//! clipboard (paste them onto a MIDI or instrument track).

use super::*;
use crate::{Clipboard, cmd};
use serde_json::json;
use soundcraft_midi::{Note, Sequence};
use soundcraft_model::{ChannelFormat, Clip, ClipContent, ClipId, Session, TrackId, TrackKind};
use soundcraft_time::{Range, TICKS_PER_QUARTER};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(noundo "edit.copy_audio_as_midi", "Audio as MIDI", ["Edit", "Copy Special"], None, "{track?, start?, end?} — monophonic pitch tracking of the selected audio", has_range, audio_as_midi),
        cmd!("audio.detect_bends", "Detect Bend Markers", ["Edit"], None, "{track?, clip?, sensitivity?: 0.65} — detect and display audio onsets", has_tracks, detect_bends),
        cmd!("audio.extract_drums", "Extract Drums to MIDI", ["Edit"], None, "{track?, clip?, to_track?, sensitivity?: 0.65, pitch?: 36} — create an instrument track when no destination is supplied", has_tracks, extract_drums),
    ]
}

fn audio_as_midi(e: &mut Engine, p: &Value) -> Result<Value> {
    let id = "edit.copy_audio_as_midi";
    let t = tracks_required(e, id, p)?.first().copied().ok_or_else(|| bad(id, "no track"))?;
    let r = range_param(e, id, p)?;
    if r.is_empty() {
        return Err(bad(id, "select some audio first"));
    }
    let s = e.session();
    if s.track(t).is_none_or(|x| x.kind != soundcraft_model::TrackKind::Audio) {
        return Err(bad(id, "select audio on an audio track"));
    }
    let r = crate::io::bounded_to_track(s, t, r);
    let audio = soundcraft_mix::render_clips(s, t, r);
    let n = audio.first().map_or(0, Vec::len);
    let k = audio.len().max(1) as f32;
    let mono: Vec<f32> = (0..n).map(|i| audio.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / k).collect();
    let detected = soundcraft_dsp::pitch_detect::audio_to_notes(&mono, s.sample_rate.as_f64() as f32);
    let base = s.tempo.samples_to_ticks(r.start, s.sample_rate);
    let mut seq = Sequence::default();
    for d in &detected {
        let a = s.tempo.samples_to_ticks(r.start + d.start as i64, s.sample_rate) - base;
        let b = s.tempo.samples_to_ticks(r.start + d.end as i64, s.sample_rate) - base;
        seq.notes.push(Note { pitch: d.pitch, velocity: d.velocity, release_velocity: 64, channel: 0, start: a.max(0), length: (b - a).max(1) });
    }
    seq.sort();
    let count = seq.notes.len();
    let clip = Clip::midi(ClipId(0), "Audio as MIDI", 0, r.len(), seq);
    e.clipboard = Clipboard { length: r.len(), tracks: vec![vec![clip]], automation: vec![Vec::new()], ..Clipboard::default() };
    Ok(json!({"notes": count, "length": r.len()}))
}

fn source_and_range(e: &Engine, p: &Value, cmd: &str) -> Result<(TrackId, Range)> {
    let s = e.session();
    if let Some(cid) = p.get("clip").and_then(Value::as_u64) {
        let (t, c) = s.find_clip(ClipId(cid)).ok_or_else(|| bad(cmd, "audio clip not found"))?;
        if !matches!(&c.content, ClipContent::Audio { .. }) {
            return Err(bad(cmd, "source must be an audio clip"));
        }
        return Ok((t, c.range()));
    }
    let track = tracks_required(e, cmd, p)?.first().copied().ok_or_else(|| bad(cmd, "select an audio track"))?;
    if s.track(track).is_none_or(|t| t.kind != TrackKind::Audio) {
        return Err(bad(cmd, "source must be an audio track"));
    }
    let tr = s.track(track).ok_or_else(|| bad(cmd, "audio track not found"))?;
    let start = tr.clips().iter().map(|c| c.start).min().ok_or_else(|| bad(cmd, "no audio clips"))?;
    let end = tr.clips().iter().map(Clip::end).max().unwrap_or(start);
    Ok((track, Range::new(start, end)))
}

/// Per-onset relative velocity (from the following 25 ms of audio), normalized across hits.
fn drum_onsets(s: &Session, track: TrackId, r: Range, sensitivity: f32) -> Vec<(i64, u8)> {
    let audio = soundcraft_mix::render_clips(s, track, r);
    let sr = s.sample_rate.as_f64() as f32;
    let peaks: Vec<(i64, f32)> = soundcraft_dsp::offline::detect_transients(&audio, sr, sensitivity)
        .into_iter().map(|sample| {
            let window = (sr * 0.025).max(1.0) as usize;
            let end = sample.saturating_add(window);
            let amplitude = audio.iter().flat_map(|ch| ch.get(sample..end.min(ch.len())).unwrap_or(&[]).iter())
                .filter(|v| v.is_finite()).fold(0.0f32, |m, v| m.max(v.abs()));
            (r.start.saturating_add(sample as i64), amplitude)
        }).collect();
    let max_peak = peaks.iter().map(|(_, amp)| *amp).fold(0.0f32, f32::max);
    if max_peak <= 1e-6 { return Vec::new(); }
    peaks.into_iter().filter(|(_, amp)| *amp >= max_peak * 0.02)
        .map(|(at, amp)| (at, (20.0 + 107.0 * (amp / max_peak).sqrt()).round().clamp(1.0, 127.0) as u8))
        .collect()
}

fn annotate_bends(s: &mut Session, track: TrackId, hits: &[(i64, u8)]) {
    if let Some(tr) = s.track_mut(track) {
        if let Some(pl) = tr.playlist_mut() {
            for clip in &mut pl.clips {
                if matches!(&clip.content, ClipContent::Audio { .. }) {
                    clip.bend_markers = hits.iter().filter(|(at, _)| *at >= clip.start && *at < clip.end())
                        .map(|(at, _)| at.saturating_sub(clip.start)).collect();
                }
            }
        }
    }
}

fn detect_bends(e: &mut Engine, p: &Value) -> Result<Value> {
    let cmd = "audio.detect_bends";
    let (track, range) = source_and_range(e, p, cmd)?;
    if range.len() <= 0 || range.len() > e.session().sample_rate.samples(600.0) {
        return Err(bad(cmd, "select up to ten minutes of audio"));
    }
    let hits = drum_onsets(e.session(), track, range, f32_or(p, "sensitivity", 0.65).clamp(0.0, 1.0));
    annotate_bends(e.session_mut(), track, &hits);
    Ok(json!({"track": track, "markers": hits.len()}))
}

fn extract_drums(e: &mut Engine, p: &Value) -> Result<Value> {
    let cmd = "audio.extract_drums";
    let (track, range) = source_and_range(e, p, cmd)?;
    let duration = range.len();
    if duration <= 0 || duration > e.session().sample_rate.samples(600.0) {
        return Err(bad(cmd, "select up to ten minutes of audio"));
    }
    let hits = drum_onsets(e.session(), track, range, f32_or(p, "sensitivity", 0.65).clamp(0.0, 1.0));
    if hits.is_empty() {
        return Err(bad(cmd, "no drum transients detected"));
    }
    let s = e.session_mut();
    let dest = if let Some(v) = p.get("to_track") {
        let id = v.as_u64().map(TrackId).or_else(|| v.as_str().and_then(|name| s.track_by_name(name).map(|t| t.id)))
            .ok_or_else(|| bad(cmd, "unknown destination track"))?;
        if s.track(id).is_none_or(|t| !t.kind.is_midi()) {
            return Err(bad(cmd, "destination must be an instrument or MIDI track"));
        }
        id
    } else {
        let name = s.track(track).map_or("Drums".to_string(), |t| format!("{} MIDI", t.name));
        let id = s.add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some(&name));
        if let Some(tr) = s.track_mut(id) {
            tr.instrument = Some(soundcraft_model::Insert::new("drum_synth"));
        }
        id
    };
    let pitch = i64_or(p, "pitch", 36).clamp(0, 127) as u8;
    let base = s.tempo.samples_to_ticks(range.start, s.sample_rate);
    let mut sequence = Sequence::default();
    for (at, velocity) in &hits {
        let tick = s.tempo.samples_to_ticks(*at, s.sample_rate).saturating_sub(base);
        sequence.notes.push(Note {
            pitch, velocity: *velocity, release_velocity: 64, channel: 9,
            start: tick.max(0), length: TICKS_PER_QUARTER / 8,
        });
    }
    sequence.sort();
    let clip_id = s.new_clip_id();
    crate::edit::place_clip(s, dest, Clip::midi(clip_id, "Extracted Drums", range.start, duration, sequence));
    annotate_bends(s, track, &hits);
    Ok(json!({"track": dest, "clip": clip_id, "notes": hits.len(), "source": track}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_audio_io::AudioBuffer;
    use soundcraft_model::{ChannelFormat, TrackKind};

    #[test]
    fn copies_a_melody_as_midi_and_pastes_it() {
        let mut e = Engine::default();
        let sr = 48_000.0f32;
        let tone = |f: f32| -> Vec<f32> { (0..(0.4 * sr) as usize).map(|i| (i as f32 * f * std::f32::consts::TAU / sr).sin() * 0.5).collect() };
        let mut a = tone(220.0);
        a.extend(tone(330.0));
        let frames = a.len() as i64;
        let s = e.session_mut();
        let src = crate::io::add_source(s, "mel", AudioBuffer { sample_rate: 48_000, channels: vec![a] }, None, soundcraft_audio_io::FileFormat::Wav);
        let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
        let cid = s.new_clip_id();
        crate::edit::place_clip(s, t, Clip::audio(cid, "mel", src, 0, 0, frames));
        let r = e.execute("edit.copy_audio_as_midi", &json!({"track": t.0, "start": 0, "end": frames})).unwrap();
        assert_eq!(r["notes"], 2, "{r}");
        let m = e.session_mut().add_track(TrackKind::Midi, ChannelFormat::Mono, None);
        e.execute("edit.paste", &json!({"track": m.0, "at": 0})).unwrap();
        let clip = e.session().track(m).unwrap().clips()[0].clone();
        match clip.content {
            soundcraft_model::ClipContent::Midi { sequence } => assert_eq!(sequence.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![57, 64]),
            _ => panic!("expected MIDI"),
        }
    }
    #[test]
    fn drum_extraction_creates_playable_instrument_with_velocities_and_markers() {
        let mut e = Engine::default();
        let sr = 48_000usize;
        let mut audio = vec![0.0f32; sr * 2];
        for (at, gain) in [(sr / 3, 0.3f32), (sr, 0.95f32)] {
            for offset in 0..2000 {
                let x = offset as f32 / 2000.0;
                audio[at + offset] = (1.0 - x) * gain * (offset as f32 * 0.17).sin();
            }
        }
        let session = e.session_mut();
        let src = crate::io::add_source(session, "Drum hits",
            AudioBuffer { sample_rate: sr as u32, channels: vec![audio] },
            None, soundcraft_audio_io::FileFormat::Wav);
        let t = session.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Drums"));
        let clip = session.new_clip_id();
        crate::edit::place_clip(session, t, Clip::audio(clip, "Drums", src, 0, 0, (sr * 2) as i64));

        let result = e.execute("audio.extract_drums", &json!({"clip": clip.0})).unwrap();
        let dest = TrackId(result["track"].as_u64().unwrap());
        let inst = e.session().track(dest).unwrap();
        assert_eq!(inst.kind, TrackKind::Instrument);
        assert_eq!(inst.instrument.as_ref().map(|x| x.plugin.as_str()), Some("drum_synth"));
        let ClipContent::Midi { sequence } = &inst.clips()[0].content else {
            panic!("expected MIDI");
        };
        assert!(sequence.notes.len() >= 2, "expected two onset notes");
        let min = sequence.notes.iter().map(|n| n.velocity).min().unwrap();
        let max = sequence.notes.iter().map(|n| n.velocity).max().unwrap();
        assert!(max > min, "the loud hit must have greater velocity");
        assert!(e.session().track(t).unwrap().clips()[0].bend_markers.len() >= 2);
    }

}
