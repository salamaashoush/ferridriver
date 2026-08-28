//! One simulated TCP connection.
//!
//! From devtools-frontend `lantern/simulation/TCPConnection.ts`. The
//! model is slow start over a congestion window measured in segments,
//! plus a handshake whose cost depends on whether the connection is
//! already warm and whether it is TLS.

/// Segments a fresh connection may send before waiting for an ack.
const INITIAL_CONGESTION_WINDOW: f64 = 10.0;

/// Bytes per TCP segment.
const TCP_SEGMENT_SIZE: f64 = 1460.0;

#[derive(Debug, Clone)]
pub struct TcpConnection {
  pub warmed: bool,
  pub ssl: bool,
  pub h2: bool,
  pub rtt: f64,
  pub throughput: f64,
  pub server_latency: f64,
  pub congestion_window: f64,
  /// h2 multiplexes, so a download can overshoot into bytes the next
  /// request on the same connection then does not have to fetch.
  pub h2_overflow_bytes_downloaded: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct DownloadOptions {
  pub dns_resolution_time: f64,
  pub time_already_elapsed: f64,
  pub maximum_time_to_elapse: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct DownloadResult {
  pub time_elapsed: f64,
  pub bytes_downloaded: f64,
  pub extra_bytes_downloaded: f64,
  pub congestion_window: f64,
}

impl TcpConnection {
  #[must_use]
  pub fn new(rtt: f64, throughput: f64, server_latency: f64, ssl: bool, h2: bool) -> Self {
    Self {
      warmed: false,
      ssl,
      h2,
      rtt,
      throughput,
      server_latency,
      congestion_window: INITIAL_CONGESTION_WINDOW,
      h2_overflow_bytes_downloaded: 0.0,
    }
  }

  /// How large the window can grow before the link, not the window, is
  /// the limit.
  fn maximum_congestion_window(&self) -> f64 {
    let bytes_per_second = self.throughput / 8.0;
    let seconds_per_round_trip = self.rtt / 1000.0;
    (bytes_per_second * seconds_per_round_trip / TCP_SEGMENT_SIZE).floor()
  }

  /// Advance the download by up to `maximum_time_to_elapse`.
  ///
  /// Returns how much time and how many bytes that took, so the caller
  /// can stop at a boundary and resume later with the window intact.
  #[must_use]
  pub fn simulate_download_until(&self, bytes_to_download: f64, options: DownloadOptions) -> DownloadResult {
    let mut bytes_to_download = bytes_to_download;
    if self.warmed && self.h2 {
      bytes_to_download -= self.h2_overflow_bytes_downloaded;
    }

    let two_way_latency = self.rtt;
    let one_way_latency = two_way_latency / 2.0;
    let maximum_congestion_window = self.maximum_congestion_window();

    // A warm connection is already open, so only the request itself has
    // to travel. A cold one pays DNS, the SYN/SYN-ACK/ACK exchange and,
    // over TLS, a further round trip (assuming False Start).
    let handshake_and_request = if self.warmed {
      one_way_latency
    } else {
      options.dns_resolution_time
        + one_way_latency
        + one_way_latency
        + one_way_latency
        + if self.ssl { two_way_latency } else { 0.0 }
    };

    let mut time_to_first_byte = handshake_and_request + self.server_latency + one_way_latency;
    // A warm h2 connection can start streaming immediately.
    if self.warmed && self.h2 {
      time_to_first_byte = 0.0;
    }

    let time_elapsed_for_ttfb = (time_to_first_byte - options.time_already_elapsed).max(0.0);
    let maximum_download_time = options.maximum_time_to_elapse - time_elapsed_for_ttfb;

    let mut congestion_window = self.congestion_window.min(maximum_congestion_window);
    // Bytes only start flowing once the headers have; if the caller has
    // already paid the TTFB, this call resumes mid-download.
    let mut total_bytes_downloaded = if time_elapsed_for_ttfb > 0.0 {
      congestion_window * TCP_SEGMENT_SIZE
    } else {
      0.0
    };

    let mut download_time_elapsed = 0.0;
    let mut bytes_remaining = bytes_to_download - total_bytes_downloaded;
    while bytes_remaining > 0.0 && download_time_elapsed <= maximum_download_time {
      download_time_elapsed += two_way_latency;
      // Slow start: the window doubles each round trip until the link
      // saturates.
      congestion_window = maximum_congestion_window.min(congestion_window * 2.0).max(1.0);
      let bytes_in_window = congestion_window * TCP_SEGMENT_SIZE;
      total_bytes_downloaded += bytes_in_window;
      bytes_remaining -= bytes_in_window;
    }

    DownloadResult {
      time_elapsed: time_elapsed_for_ttfb + download_time_elapsed,
      bytes_downloaded: total_bytes_downloaded.min(bytes_to_download).max(0.0),
      extra_bytes_downloaded: if self.h2 {
        (total_bytes_downloaded - bytes_to_download).max(0.0)
      } else {
        0.0
      },
      congestion_window,
    }
  }
}
