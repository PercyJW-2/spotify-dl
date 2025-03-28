use std::fmt::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use futures::TryStreamExt;
use indicatif::MultiProgress;
use indicatif::ProgressBar;
use indicatif::ProgressState;
use indicatif::ProgressStyle;
use librespot::core::session::Session;
use librespot::playback::config::PlayerConfig;
use librespot::playback::mixer::NoOpVolume;
use librespot::playback::mixer::VolumeGetter;
use librespot::playback::player::Player;
use tracing_subscriber::fmt::format;
use crate::channel_sink::ChannelSink;
use crate::encoder::Format;
use crate::encoder::Samples;
use crate::channel_sink::SinkEvent;
use crate::track::Track;
use crate::track::TrackMetadata;

pub struct Downloader {
    player_config: PlayerConfig,
    session: Session,
    progress_bar: MultiProgress,
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub destination: PathBuf,
    pub compression: Option<u32>,
    pub parallel: usize,
    pub format: Format,
}

impl DownloadOptions {
    pub fn new(destination: Option<String>, compression: Option<u32>, parallel: usize, format: Format) -> Self {
        let destination =
            destination.map_or_else(|| std::env::current_dir().unwrap(), PathBuf::from);
        DownloadOptions {
            destination,
            compression,
            parallel,
            format
        }
    }
}

impl Downloader {
    pub fn new(session: Session) -> Self {
        Downloader {
            player_config: PlayerConfig::default(),
            session,
            progress_bar: MultiProgress::new(),
        }
    }

    pub async fn download_tracks(
        self,
        tracks: Vec<Track>,
        options: &DownloadOptions,
    ) -> Result<()> {
        futures::stream::iter(tracks)
            .map(|track| {
                self.download_track(track, options)
            })
            .buffer_unordered(options.parallel)
            .try_collect::<Vec<_>>()
            .await?;

        Ok(())
    }

    #[tracing::instrument(name = "download_track", skip(self))]
    async fn download_track(&self, track: Track, options: &DownloadOptions) -> Result<()> {
        let metadata = track.metadata(&self.session).await?;
        tracing::info!("Downloading track: {:?}", metadata);

        let file_name = self.get_file_name(&metadata, track.track_number);
        let path = options
            .destination
            .join(file_name.clone())
            .with_extension(options.format.extension())
            .to_str()
            .ok_or(anyhow::anyhow!("Could not set the output path"))?
            .to_string();

        let (sink, mut sink_channel) = ChannelSink::new(metadata);

        let file_size = sink.get_approximate_size();

        let player = Player::new(
            self.player_config.clone(),
            self.session.clone(),
            self.volume_getter(),
            move || Box::new(sink),
        );

        let pb = self.progress_bar.add(ProgressBar::new(file_size as u64));
        pb.enable_steady_tick(Duration::from_millis(100));
        pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
            .with_key("eta", |state: &ProgressState, w: &mut dyn Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("#>-"));
        pb.set_message(file_name.clone());

        player.load(track.id, true, 0);

        let mut samples = Vec::<i32>::new();

        tokio::spawn(async move {
            player.await_end_of_track().await;
            player.stop();
        });

        while let Some(event) = sink_channel.recv().await {
            match event {
                SinkEvent::Write { bytes, total, mut content } => {
                    tracing::trace!("Written {} bytes out of {}", bytes, total);
                    pb.set_position(bytes as u64);
                    samples.append(&mut content);
                }
                SinkEvent::Finished => {
                    tracing::info!("Finished downloading track: {:?}", file_name);
                    break;
                }
            }
        }

        tracing::info!("Encoding track: {:?}", file_name);
        pb.set_message(format!("Encoding {}", &file_name));
        let samples = Samples::new(samples, 44100, 2, 16);
        let encoder = crate::encoder::get_encoder(options.format);
        let stream = encoder.encode(samples).await?;

        pb.set_message(format!("Writing {}", &file_name));
        tracing::info!("Writing track: {:?} to file: {}", file_name, &path);
        stream.write_to_file(&path).await?;

        let track_duration = track.metadata(&self.session).await?.duration as u64;
        pb.set_message(format!("Downloaded {} Waiting {}ms", &file_name, track_duration));
        tokio::time::sleep(Duration::from_millis(track_duration)).await;
        pb.finish_with_message(format!("Finished {}", &file_name));
        Ok(())
    }

    fn volume_getter(&self) -> Box<dyn VolumeGetter + Send> {
        Box::new(NoOpVolume)
    }

    fn get_file_name(&self, metadata: &TrackMetadata, track_num: usize) -> String {
        // If there is more than 3 artists, add the first 3 and add "and others" at the end
        if metadata.artists.len() > 3 {
            let artists_name = metadata
                .artists
                .iter()
                .take(3)
                .map(|artist| artist.name.clone())
                .collect::<Vec<String>>()
                .join(", ");
            return self.clean_file_name(format!(
                "{:0>4} - {}, and others - {}",
                track_num, artists_name, metadata.track_name
            ));
        }

        let artists_name = metadata
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect::<Vec<String>>()
            .join(", ");
        self.clean_file_name(format!("{:0>4} - {} - {}", track_num, artists_name, metadata.track_name))
    }

    fn clean_file_name(&self, file_name: String) -> String {
        let invalid_chars = ['<', '>', ':', '\'', '"', '/', '\\', '|', '?', '*'];
        let mut clean = String::new();

        // Everything but Windows should allow non-ascii characters
        let allows_non_ascii = !cfg!(windows);
        for c in file_name.chars() {
            if !invalid_chars.contains(&c) && (c.is_ascii() || allows_non_ascii) && !c.is_control()
            {
                clean.push(c);
            }
        }
        clean
    }
}
