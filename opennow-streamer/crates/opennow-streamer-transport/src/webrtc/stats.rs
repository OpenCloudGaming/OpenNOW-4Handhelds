use super::*;
use str0m::stats::{MediaIngressStats, PeerStats};

const PING_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default)]
pub struct TransportStats {
    pub ping_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub packet_loss_percent: Option<f64>,
    pub socket_receive_bytes: Option<u64>,
}

pub(super) struct Stats {
    video_mid: Mid,
    video_clock_rate: Frequency,
    previous_responses: u64,
    ice_ping: Option<(Instant, Duration)>,
    /// RTCP XR DLRR round trip for the video stream.
    media_ping: Option<(Instant, Duration)>,
    /// Transport-wide feedback round trip from the peer stats event.
    peer_ping: Option<(Instant, Duration)>,
    peer_loss: Option<f32>,
    video_loss: Option<f32>,
    latest: TransportStats,
}

impl Stats {
    pub(super) fn new(video_mid: Mid, video_clock_rate: Frequency) -> Self {
        Self {
            video_mid,
            video_clock_rate,
            previous_responses: 0,
            ice_ping: None,
            media_ping: None,
            peer_ping: None,
            peer_loss: None,
            video_loss: None,
            latest: TransportStats::default(),
        }
    }

    pub(super) fn peer(&mut self, stats: PeerStats) -> TransportStats {
        match stats.selected_candidate_pair {
            None => {
                self.previous_responses = 0;
                self.ice_ping = None;
            }
            Some(pair) if pair.responses_received != self.previous_responses => {
                self.previous_responses = pair.responses_received;
                self.ice_ping = pair
                    .current_round_trip_time
                    .filter(|rtt| !rtt.is_zero())
                    .map(|rtt| (stats.timestamp, rtt));
            }
            Some(_) => {}
        }
        if let Some(rtt) = stats.rtt.filter(|rtt| !rtt.is_zero()) {
            self.peer_ping = Some((stats.timestamp, rtt));
        }
        self.latest.ping_ms = self.ping_ms(stats.timestamp);
        self.latest.socket_receive_bytes = Some(stats.peer_bytes_rx);
        self.peer_loss = stats.ingress_loss_fraction;
        self.latest.packet_loss_percent = self
            .video_loss
            .or(self.peer_loss)
            .map(|loss| f64::from(loss) * 100.0);
        self.latest
    }

    pub(super) fn media(&mut self, stats: MediaIngressStats) -> Option<TransportStats> {
        if stats.mid != self.video_mid {
            return None;
        }
        self.latest.jitter_ms =
            Some(f64::from(stats.jitter) * 1_000.0 / f64::from(self.video_clock_rate.get()));
        if let Some(rtt) = stats.rtt.filter(|rtt| !rtt.is_zero()) {
            self.media_ping = Some((stats.timestamp, rtt));
        }
        self.latest.ping_ms = self.ping_ms(stats.timestamp);
        self.video_loss = stats.loss;
        self.latest.packet_loss_percent = self
            .video_loss
            .or(self.peer_loss)
            .map(|loss| f64::from(loss) * 100.0);
        Some(self.latest)
    }

    /// Smallest fresh round trip. A consent check can read about two seconds
    /// while the media path is a few hundred milliseconds.
    fn ping_ms(&self, now: Instant) -> Option<f64> {
        [self.media_ping, self.peer_ping, self.ice_ping]
            .into_iter()
            .filter_map(|sample| {
                sample
                    .filter(|(at, _)| now.saturating_duration_since(*at) < PING_TIMEOUT)
                    .map(|(_, rtt)| rtt)
            })
            .min()
            .map(|rtt| rtt.as_secs_f64() * 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use str0m::stats::{CandidatePairStats, CandidateStats};

    fn pair(rtt: Duration, responses: u64) -> CandidatePairStats {
        CandidatePairStats {
            protocol: Protocol::Udp,
            local: CandidateStats {
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            remote: CandidateStats {
                addr: "127.0.0.1:2".parse().unwrap(),
            },
            current_round_trip_time: Some(rtt),
            total_round_trip_time: Duration::ZERO,
            responses_received: responses,
        }
    }

    #[test]
    fn ping_prefers_the_media_round_trip_over_a_slow_consent_check() {
        let now = Instant::now();
        let mut stats = Stats::new(Mid::from("0"), Frequency::NINETY_KHZ);
        let shown = stats.peer(PeerStats {
            peer_bytes_rx: 0,
            peer_bytes_tx: 0,
            bytes_rx: 0,
            bytes_tx: 0,
            timestamp: now,
            bwe_tx: None,
            egress_loss_fraction: None,
            ingress_loss_fraction: None,
            rtt: None,
            selected_candidate_pair: Some(pair(Duration::from_millis(2_100), 1)),
        });
        assert_eq!(shown.ping_ms, Some(2_100.0));
        let shown = stats
            .media(MediaIngressStats {
                mid: Mid::from("0"),
                rid: None,
                bytes: 1,
                packets: 1,
                firs: 0,
                plis: 0,
                nacks: 0,
                jitter: 0,
                rtt: Some(Duration::from_millis(240)),
                loss: None,
                timestamp: now,
                remote: None,
            })
            .expect("video stats");
        assert_eq!(shown.ping_ms, Some(240.0));
    }
}
