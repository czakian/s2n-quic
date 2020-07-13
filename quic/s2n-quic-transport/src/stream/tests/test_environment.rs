use super::{
    gen_pattern_test_data, stream_data, MockConnectionContext, MockWriteContext,
    OutgoingFrameBuffer,
};
use crate::{
    frame_exchange_interests::FrameExchangeInterests,
    stream::{
        incoming_connection_flow_controller::IncomingConnectionFlowController,
        outgoing_connection_flow_controller::OutgoingConnectionFlowController,
        stream_impl::StreamConfig, stream_interests::StreamInterests, StreamEvents, StreamImpl,
        StreamTrait,
    },
};
use bytes::Bytes;
use core::task::{Context, Poll, Waker};
use futures_test::task::{new_count_waker, AwokenCount};
use s2n_quic_core::{
    application::ApplicationErrorCode,
    endpoint::EndpointType,
    frame::{Frame, ResetStream},
    packet::number::{PacketNumber, PacketNumberSpace},
    stream::{StreamId, StreamType},
    time::Timestamp,
    varint::VarInt,
};

/// Defines whether a wakeup is expected.
/// `None` means there are no expectations. `Some(true)` expects a wakeup,
/// `Some(false)` does not.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct ExpectWakeup(pub Option<bool>);

/// Creates an application space packet number with the given value
pub fn pn(nr: usize) -> PacketNumber {
    PacketNumberSpace::ApplicationData.new_packet_number(VarInt::new(nr as u64).unwrap())
}

/// Creates Stream Interests from an array of strings
///
/// The following interests are supported:
/// - ack => frame_exchange.delivery_notifications
/// - tx => frame_exchange.transmission
/// - fin => finalization
/// - cf => connection_flow_control_credits
pub fn stream_interests(interests: &[&str]) -> StreamInterests {
    let mut result = StreamInterests::default();
    for interest in interests {
        match *interest {
            "ack" => result.frame_exchange.delivery_notifications = true,
            "tx" => result.frame_exchange.transmission = true,
            "fin" => result.finalization = true,
            "cf" => result.connection_flow_control_credits = true,
            other => unreachable!("Unsupported interest {}", other),
        }
    }
    result
}

/// Creates Stream Interests from an array of strings
///
/// The following interests are supported:
/// - ack => frame_exchange.delivery_notifications
/// - tx => frame_exchange.transmission
pub fn frame_exchange_interests(interests: &[&str]) -> FrameExchangeInterests {
    let mut result = FrameExchangeInterests::default();
    for interest in interests {
        match *interest {
            "ack" => result.delivery_notifications = true,
            "tx" => result.transmission = true,
            other => unreachable!("Unsupported interest {}", other),
        }
    }
    result
}

/// Holds a set of associated objects that act as a test environment for a single
/// [`StreamImpl`].
pub struct TestEnvironment {
    pub connection_context: MockConnectionContext,
    pub sent_frames: OutgoingFrameBuffer,
    pub stream: StreamImpl,
    pub rx_connection_flow_controller: IncomingConnectionFlowController,
    pub tx_connection_flow_controller: OutgoingConnectionFlowController,
    pub wake_counter: AwokenCount,
    pub waker: Waker,
    pub current_time: Timestamp,
}

impl TestEnvironment {
    // These are the defaults for configuration values which are utilized for
    // most tests.
    // In order to test that config values are not accidentally mixed up in the
    // library, we use slightly different values for those. The exact numbers
    // should not matter too much - higher numbers will require tests to take
    // longer.

    pub const DEFAULT_INITIAL_CONNECTION_RECEIVE_WINDOW: u64 = 100 * 1024;
    pub const DEFAULT_INITIAL_CONNECTION_SEND_WINDOW: u64 = 100 * 1024;
    pub const DEFAULT_INITIAL_RECEIVE_WINDOW: u64 = 4096;
    pub const DEFAULT_INITIAL_SEND_WINDOW: u64 = 8 * 1024;
    pub const DEFAULT_MAX_SEND_BUFFER_SIZE: usize = 16 * 1024;

    /// Asserts that the given byte array can be read from the stream
    pub fn assert_receive_data(&mut self, data: &'static [u8]) {
        let poll_context = Context::from_waker(&self.waker);
        assert_eq!(
            Poll::Ready(Ok(Some(Bytes::from_static(data)))),
            self.stream
                .poll_pop(&self.connection_context, &poll_context)
        );
    }

    /// Asserts that no data is available for reading and that the stream
    /// has not yet finished
    pub fn assert_no_read_data(&mut self) {
        let poll_context = Context::from_waker(&self.waker);
        assert_eq!(
            Poll::Pending,
            self.stream
                .poll_pop(&self.connection_context, &poll_context)
        );
    }

    /// Asserts that the stream has reached end of stream
    pub fn assert_end_of_stream(&mut self) {
        let poll_context = Context::from_waker(&self.waker);
        assert_eq!(
            Poll::Ready(Ok(None)),
            self.stream
                .poll_pop(&self.connection_context, &poll_context)
        );
    }

    /// Asserts that the returns an error when pop is called
    pub fn assert_pop_error(&mut self) {
        let poll_context = Context::from_waker(&self.waker);

        let poll_result = self
            .stream
            .poll_pop(&self.connection_context, &poll_context);

        match poll_result {
            Poll::Ready(Err(_)) => {}
            _ => panic!("Expected the stream to be reset"),
        }
    }

    /// Feeds `amount` of bytes into the stream at the given offset
    pub fn feed_data(&mut self, mut offset: VarInt, amount: usize) {
        let mut remaining = amount;
        while remaining > 0 {
            let to_write = core::cmp::min(remaining, 4096);
            let data = vec![0u8; to_write];
            let mut events = StreamEvents::new();
            assert!(self
                .stream
                .on_data(
                    &stream_data(self.stream.stream_id, offset, &data[..], false),
                    &mut events
                )
                .is_ok());
            offset += VarInt::from_u32(to_write as u32);
            remaining -= to_write;
        }
    }

    /// Consumes all currently available data from the stream
    pub fn consume_all_data(&mut self) -> usize {
        let mut result = 0;
        let poll_context = Context::from_waker(&self.waker);
        loop {
            let poll_result = self
                .stream
                .poll_pop(&self.connection_context, &poll_context);

            match poll_result {
                Poll::Pending => break, // Consumed all data
                Poll::Ready(Ok(Some(data))) => {
                    result += data.len();
                }
                Poll::Ready(Ok(None)) => break, // Consumed all data to end of stream
                _ => panic!("Unexpected read result {:?}", poll_result),
            }
        }

        result
    }

    /// Queries the stream for pending outgoing frames.
    /// Asserts that `expected_frames` had been written.
    /// The frames will get written into `sent_frames`.
    pub fn assert_write_frames(&mut self, expected_frames: usize) {
        let prev_written = self.sent_frames.len();
        let mut write_ctx = MockWriteContext::new(
            &self.connection_context,
            self.current_time,
            &mut self.sent_frames,
        );
        assert!(self
            .rx_connection_flow_controller
            .on_transmit(&mut write_ctx)
            .is_ok());
        assert!(self.stream.on_transmit(&mut write_ctx).is_ok());
        self.sent_frames.flush();
        assert_eq!(
            prev_written + expected_frames,
            self.sent_frames.len(),
            "Unexpected amount of written frames"
        );
    }

    /// Asserts that a stream data frame for a given offset and size was emitted
    pub fn assert_write_of(
        &mut self,
        expected_offset: VarInt,
        expected_size: usize,
        expect_eof: bool,
        expect_is_last_frame: bool,
        expected_packet_number: PacketNumber,
    ) {
        let mut write_ctx = MockWriteContext::new(
            &self.connection_context,
            self.current_time,
            &mut self.sent_frames,
        );
        assert!(self.stream.on_transmit(&mut write_ctx).is_ok());
        self.sent_frames.flush();

        let mut sent_frame = self.sent_frames.pop_front().expect("Frame is written");
        assert_eq!(
            expected_packet_number, sent_frame.packet_nr,
            "packet number mismatch"
        );

        let decoded_frame = sent_frame.as_frame();
        // These assertions are on individual fields to see more easily
        // where things are failing.
        if let Frame::Stream(mut stream_frame) = decoded_frame {
            assert_eq!(expected_offset, stream_frame.offset, "offset mismatch");
            assert_eq!(expect_eof, stream_frame.is_fin, "FIN mismatch");
            assert_eq!(expected_size, stream_frame.data.len(), "size mismatch");
            assert_eq!(
                expect_is_last_frame, stream_frame.is_last_frame,
                "is_last_frame mismatch"
            );
            assert_eq!(
                gen_pattern_test_data(expected_offset, expected_size),
                stream_frame.data.as_less_safe_slice_mut(),
                "data mismatch",
            );
        } else {
            panic!("Expected a Stream frame, but got {:?}", decoded_frame);
        }
    }

    /// Asserts that a RESET frame was transmitted
    pub fn assert_write_reset_frame(
        &mut self,
        expected_error_code: ApplicationErrorCode,
        expected_packet_number: PacketNumber,
        expected_final_size: VarInt,
    ) {
        let mut write_ctx = MockWriteContext::new(
            &self.connection_context,
            self.current_time,
            &mut self.sent_frames,
        );
        assert!(self.stream.on_transmit(&mut write_ctx).is_ok());
        self.sent_frames.flush();

        let mut sent_frame = self.sent_frames.pop_front().expect("Frame is written");
        assert_eq!(
            expected_packet_number, sent_frame.packet_nr,
            "packet number mismatch"
        );

        assert_eq!(
            Frame::ResetStream(ResetStream {
                stream_id: self.stream.stream_id.into(),
                application_error_code: expected_error_code.into(),
                final_size: expected_final_size,
            }),
            sent_frame.as_frame()
        );
    }

    /// Acknowledges a packet with a given packet number as received.
    /// `expect_writer_wakeup` specifies whether we expect the wake counter to
    /// get increased due to this operation.
    pub fn ack_packet(&mut self, packet_number: PacketNumber, expect_writer_wakeup: ExpectWakeup) {
        let old_wake_count = self.wake_counter.get();
        self.rx_connection_flow_controller
            .on_packet_ack(&packet_number);
        let mut events = StreamEvents::new();
        self.stream.on_packet_ack(&packet_number, &mut events);
        events.wake_all();
        let new_wake_count = self.wake_counter.get();
        let was_woken = new_wake_count > old_wake_count;
        if let ExpectWakeup(Some(wakeup_expected)) = expect_writer_wakeup {
            assert_eq!(wakeup_expected, was_woken, "Unexpected wakeup through ACK");
        }
    }

    /// Declares a packet with a given packet number as lost
    pub fn nack_packet(&mut self, packet_number: PacketNumber) {
        self.rx_connection_flow_controller
            .on_packet_loss(&packet_number);
        let mut events = StreamEvents::new();
        self.stream.on_packet_loss(&packet_number, &mut events);
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct TestEnvironmentConfig {
    pub local_endpoint_type: EndpointType,
    pub stream_id: StreamId,
    pub initial_receive_window: u64,
    pub desired_flow_control_window: u32,
    pub initial_send_window: u64,
    pub initial_connection_send_window_size: u64,
    pub initial_connection_receive_window_size: u64,
    pub desired_connection_flow_control_window: u32,
    pub max_send_buffer_size: usize,
}

impl Default for TestEnvironmentConfig {
    fn default() -> TestEnvironmentConfig {
        TestEnvironmentConfig {
            local_endpoint_type: EndpointType::Server,
            stream_id: StreamId::initial(EndpointType::Client, StreamType::Bidirectional),
            initial_receive_window: TestEnvironment::DEFAULT_INITIAL_RECEIVE_WINDOW,
            desired_flow_control_window: TestEnvironment::DEFAULT_INITIAL_RECEIVE_WINDOW as u32,
            initial_send_window: TestEnvironment::DEFAULT_INITIAL_SEND_WINDOW,
            initial_connection_send_window_size:
                TestEnvironment::DEFAULT_INITIAL_CONNECTION_SEND_WINDOW,
            initial_connection_receive_window_size:
                TestEnvironment::DEFAULT_INITIAL_CONNECTION_RECEIVE_WINDOW,
            desired_connection_flow_control_window:
                TestEnvironment::DEFAULT_INITIAL_CONNECTION_RECEIVE_WINDOW as u32,
            max_send_buffer_size: TestEnvironment::DEFAULT_MAX_SEND_BUFFER_SIZE,
        }
    }
}

/// Sets up a test environment for Stream testing with default parameters
pub fn setup_stream_test_env() -> TestEnvironment {
    let config = TestEnvironmentConfig::default();
    setup_stream_test_env_with_config(config)
}

/// Sets up a test environment for Stream testing with custom parameters
pub fn setup_stream_test_env_with_config(config: TestEnvironmentConfig) -> TestEnvironment {
    let connection_context = MockConnectionContext::new(config.local_endpoint_type);

    let rx_connection_flow_controller = IncomingConnectionFlowController::new(
        VarInt::new(config.initial_connection_receive_window_size).unwrap(),
        config.desired_connection_flow_control_window,
    );

    let tx_connection_flow_controller = OutgoingConnectionFlowController::new(
        VarInt::new(config.initial_connection_send_window_size).unwrap(),
    );

    let stream = StreamImpl::new(StreamConfig {
        incoming_connection_flow_controller: rx_connection_flow_controller.clone(),
        outgoing_connection_flow_controller: tx_connection_flow_controller.clone(),
        local_endpoint_type: config.local_endpoint_type,
        stream_id: config.stream_id,
        initial_receive_window: VarInt::new(config.initial_receive_window).unwrap(),
        desired_flow_control_window: config.desired_flow_control_window,
        initial_send_window: VarInt::new(config.initial_send_window).unwrap(),
        max_send_buffer_size: config.max_send_buffer_size as u32,
    });

    let (waker, wake_counter) = new_count_waker();

    TestEnvironment {
        connection_context,
        sent_frames: OutgoingFrameBuffer::new(),
        stream,
        rx_connection_flow_controller,
        tx_connection_flow_controller,
        wake_counter,
        waker,
        current_time: s2n_quic_platform::time::now(),
    }
}