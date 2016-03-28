//! Tests for the `http::client` module

use super::{ClientSession, write_preface, RequestStream};

use http::{Header, ErrorCode, HttpError};
use http::tests::common::{TestStream, build_mock_client_conn, build_mock_http_conn,
                          MockReceiveFrame, MockSendFrame};
use http::frame::{SettingsFrame, DataFrame, Frame, RawFrame};
use http::connection::{HttpFrame, SendStatus};
use http::session::{Session, SessionState, Stream, DefaultSessionState};
use http::session::Client as ClientMarker;

/// Tests that a client connection is correctly initialized, by reading the
/// server preface (i.e. a settings frame) as the first frame of the connection.
#[test]
fn test_init_client_conn() {
    let frames = vec![HttpFrame::SettingsFrame(SettingsFrame::new())];
    let mut conn = build_mock_client_conn();
    let mut sender = MockSendFrame::new();
    let mut receiver = MockReceiveFrame::new(frames);

    conn.expect_settings(&mut receiver, &mut sender).unwrap();

    // We have read the server's response (the settings frame only, since no panic
    // ocurred)
    assert_eq!(receiver.recv_list.len(), 0);
    // We also sent an ACK already.
    assert_eq!(sender.sent.len(), 1);
    let frame = match HttpFrame::from_raw(&sender.sent[0]).unwrap() {
        HttpFrame::SettingsFrame(frame) => frame,
        _ => panic!("ACK not sent!"),
    };
    assert!(frame.is_ack());
}

/// Tests that a client connection fails to initialize when the server does
/// not send a settings frame as its first frame (i.e. server preface).
#[test]
fn test_init_client_conn_no_settings() {
    let frames = vec![HttpFrame::DataFrame(DataFrame::new(1))];
    let mut conn = build_mock_client_conn();
    let mut sender = MockSendFrame::new();
    let mut receiver = MockReceiveFrame::new(frames);

    // We get an error since the first frame sent by the server was not
    // SETTINGS.
    assert!(conn.expect_settings(&mut receiver, &mut sender).is_err());
}

/// A helper function that prepares a `TestStream` with an optional outgoing data stream.
fn prepare_stream(data: Option<Vec<u8>>) -> TestStream {
    let mut stream = TestStream::new();
    match data {
        None => stream.close_local(),
        Some(d) => stream.set_outgoing(d),
    };
    return stream;
}

/// Tests that the `ClientConnection` correctly sends the next data, depending on the streams
/// known to it.
#[test]
fn test_client_conn_send_next_data() {
    {
        // No streams => nothing sent.
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();
        let res = conn.send_next_data(&mut sender).unwrap();
        assert_eq!(res, SendStatus::Nothing);
    }
    {
        // A locally closed stream (i.e. nothing to send)
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();
        conn.state.insert_outgoing(prepare_stream(None));
        let res = conn.send_next_data(&mut sender).unwrap();
        assert_eq!(res, SendStatus::Nothing);
    }
    {
        // A stream with some data
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();
        conn.state.insert_outgoing(prepare_stream(Some(vec![1, 2, 3])));
        let res = conn.send_next_data(&mut sender).unwrap();
        assert_eq!(res, SendStatus::Sent);

        // All of it got sent in the first go, so now we've got nothing?
        let res = conn.send_next_data(&mut sender).unwrap();
        assert_eq!(res, SendStatus::Nothing);
    }
    {
        // Multiple streams with data
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();
        conn.state.insert_outgoing(prepare_stream(Some(vec![1, 2, 3])));
        conn.state.insert_outgoing(prepare_stream(Some(vec![1, 2, 3])));
        conn.state.insert_outgoing(prepare_stream(Some(vec![1, 2, 3])));
        for _ in 0..3 {
            let res = conn.send_next_data(&mut sender).unwrap();
            assert_eq!(res, SendStatus::Sent);
        }
        // All of it got sent in the first go, so now we've got nothing?
        let res = conn.send_next_data(&mut sender).unwrap();
        assert_eq!(res, SendStatus::Nothing);
    }
}

/// Tests that the `ClientConnection::start_request` method correctly starts a new request.
#[test]
fn test_client_conn_start_request() {
    {
        // No body
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();

        let stream = RequestStream {
            headers: vec![
                Header::new(b":method", b"GET"),
            ],
            stream: prepare_stream(None),
        };
        conn.start_request(stream, &mut sender).unwrap();

        // The stream is in the connection state?
        assert!(conn.state.get_stream_ref(1).is_some());
        // The headers got sent?
        // (It'd be so much nicer to assert that the `send_headers` method got called)
        assert_eq!(sender.sent.len(), 1);
        match HttpFrame::from_raw(&sender.sent[0]).unwrap() {
            HttpFrame::HeadersFrame(ref frame) => {
                // The frame closed the stream?
                assert!(frame.is_end_of_stream());
            }
            _ => panic!("Expected a Headers frame"),
        };
    }
    {
        // With a body
        let mut conn = build_mock_client_conn();
        let mut sender = MockSendFrame::new();

        let stream = RequestStream {
            headers: vec![
                Header::new(b":method", b"POST"),
            ],
            stream: prepare_stream(Some(vec![1, 2, 3])),
        };
        conn.start_request(stream, &mut sender).unwrap();

        // The stream is in the connection state?
        assert!(conn.state.get_stream_ref(1).is_some());
        // The headers got sent?
        // (It'd be so much nicer to assert that the `send_headers` method got called)
        assert_eq!(sender.sent.len(), 1);
        match HttpFrame::from_raw(&sender.sent.remove(0)).unwrap() {
            HttpFrame::HeadersFrame(ref frame) => {
                // The stream is still open
                assert!(!frame.is_end_of_stream());
            }
            _ => panic!("Expected a Headers frame"),
        };
    }
}

/// Tests that a `ClientSession` notifies the correct stream when the
/// appropriate callback is invoked.
///
/// A better unit test would give a mock Stream to the `ClientSession`,
/// instead of testing both the `ClientSession` and the `DefaultStream`
/// in the same time...
#[test]
fn test_client_session_notifies_stream() {
    let mut state = DefaultSessionState::<ClientMarker, TestStream>::new();
    state.insert_outgoing(TestStream::new());
    let mut conn = build_mock_http_conn();
    let mut sender = MockSendFrame::new();

    {
        // Registering some data to stream 1...
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.new_data_chunk(1, &[1, 2, 3], &mut conn).unwrap();
    }
    // ...works.
    assert_eq!(state.get_stream_ref(1).unwrap().body, vec![1, 2, 3]);
    {
        // Some more...
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.new_data_chunk(1, &[4], &mut conn).unwrap();
    }
    // ...works.
    assert_eq!(state.get_stream_ref(1).unwrap().body, vec![1, 2, 3, 4]);
    // Now headers?
    let headers = vec![
        Header::new(b":method", b"GET"),
    ];
    {
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.new_headers(1, headers.clone(), &mut conn).unwrap();
    }
    assert_eq!(state.get_stream_ref(1).unwrap().headers.clone().unwrap(),
               headers);
    // Add another stream in the mix
    state.insert_outgoing(TestStream::new());
    {
        // and send it some data
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.new_data_chunk(3, &[100], &mut conn).unwrap();
    }
    assert_eq!(state.get_stream_ref(3).unwrap().body, vec![100]);
    {
        // Finally, the stream 1 ends...
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.end_of_stream(1, &mut conn).unwrap();
    }
    // ...and gets closed.
    assert!(state.get_stream_ref(1).unwrap().is_closed());
    // but not the other one.
    assert!(!state.get_stream_ref(3).unwrap().is_closed());
    // Sanity check: both streams still found in the session
    assert_eq!(state.iter().collect::<Vec<_>>().len(), 2);
    // The closed stream is returned...
    let closed = state.get_closed();
    assert_eq!(closed.len(), 1);
    // ...and is also removed from the session!
    assert_eq!(state.iter().collect::<Vec<_>>().len(), 1);
}

/// Tests that the `ClientSession` notifies the correct stream when it is reset by the peer.
#[test]
fn test_client_session_on_rst_stream() {
    let mut state = DefaultSessionState::<ClientMarker, TestStream>::new();
    state.insert_outgoing(TestStream::new());
    state.insert_outgoing(TestStream::new());
    let mut conn = build_mock_http_conn();
    let mut sender = MockSendFrame::new();
    {
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.rst_stream(3, ErrorCode::Cancel, &mut conn).unwrap();
    }
    assert!(state.get_stream_ref(3)
                 .map(|stream| {
                     stream.errors.len() == 1 && stream.errors[0] == ErrorCode::Cancel
                 })
                 .unwrap());
    assert!(state.get_stream_ref(1).map(|stream| stream.errors.len() == 0).unwrap());
}

/// Tests that the `ClientSession` signals the correct error to client code when told to go
/// away by the peer.
#[test]
fn test_client_session_on_goaway() {
    let mut state = DefaultSessionState::<ClientMarker, TestStream>::new();
    let mut conn = build_mock_http_conn();
    let mut sender = MockSendFrame::new();
    let res = {
        let mut session = ClientSession::new(&mut state, &mut sender);
        session.on_goaway(0, ErrorCode::ProtocolError, None, &mut conn)
    };
    if let Err(HttpError::PeerConnectionError(err)) = res {
        assert_eq!(err.error_code(), ErrorCode::ProtocolError);
        assert_eq!(err.debug_data(), None);
    } else {
        panic!("Expected a PeerConnectionError");
    }
}

/// Tests that the `write_preface` function correctly writes a client preface to
/// a given `io::Write`.
#[test]
fn test_write_preface() {
    // The buffer (`io::Write`) into which we will write the preface.
    let mut written: Vec<u8> = Vec::new();

    // Do it...
    write_preface(&mut written).unwrap();

    // The first bytes written to the underlying transport layer are the
    // preface bytes.
    let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let frames_buf = &written[preface.len()..];
    // Immediately after that we sent a settings frame...
    assert_eq!(preface, &written[..preface.len()]);
    let raw = RawFrame::parse(frames_buf).unwrap();
    let frame: SettingsFrame = Frame::from_raw(&raw).unwrap();
    // ...which was not an ack, but our own settings.
    assert!(!frame.is_ack());
}
