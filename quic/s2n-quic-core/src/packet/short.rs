use crate::{
    crypto::{CryptoError, EncryptedPayload, HeaderCrypto, OneRTTCrypto, ProtectedPayload},
    packet::{
        decoding::HeaderDecoder,
        encoding::{PacketEncoder, PacketPayloadEncoder},
        number::{
            PacketNumber, PacketNumberLen, PacketNumberSpace, ProtectedPacketNumber,
            TruncatedPacketNumber,
        },
        DestinationConnectionIDDecoder, Tag,
    },
};
use s2n_codec::{CheckedRange, DecoderBufferMut, DecoderBufferMutResult, Encoder, EncoderValue};

//= https://tools.ietf.org/html/draft-ietf-quic-transport-22#section-17.3
//# 17.3.  Short Header Packets
//#
//#    This version of QUIC defines a single packet type which uses the
//#    short packet header.
//#
//#     0                   1                   2                   3
//#     0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//#    +-+-+-+-+-+-+-+-+
//#    |0|1|S|R|R|K|P P|
//#    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#    |                Destination Connection ID (0..160)           ...
//#    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#    |                     Packet Number (8/16/24/32)              ...
//#    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#    |                     Protected Payload (*)                   ...
//#    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#
//#                    Figure 14: Short Header Packet Format
//#
//#    The short header can be used after the version and 1-RTT keys are
//#    negotiated.  Packets that use the short header contain the following
//#    fields:
//#
//#    Header Form:  The most significant bit (0x80) of byte 0 is set to 0
//#       for the short header.
//#
//#    Fixed Bit:  The next bit (0x40) of byte 0 is set to 1.  Packets
//#       containing a zero value for this bit are not valid packets in this
//#       version and MUST be discarded.

macro_rules! short_tag {
    () => {
        0b0100u8..=0b0111u8
    };
}

const ENCODING_TAG: u8 = 0b0100_0000;

//#    Spin Bit (S):  The third most significant bit (0x20) of byte 0 is the
//#       latency spin bit, set as described in Section 17.3.1.

const SPIN_BIT_MASK: u8 = 0x20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpinBit {
    Zero,
    One,
}

impl SpinBit {
    fn from_tag(tag: Tag) -> Self {
        if tag & SPIN_BIT_MASK == SPIN_BIT_MASK {
            Self::One
        } else {
            Self::Zero
        }
    }

    fn into_packet_tag_mask(self) -> u8 {
        match self {
            Self::One => SPIN_BIT_MASK,
            Self::Zero => 0,
        }
    }
}

//#    Reserved Bits (R):  The next two bits (those with a mask of 0x18) of
//#       byte 0 are reserved.  These bits are protected using header
//#       protection (see Section 5.4 of [QUIC-TLS]).  The value included
//#       prior to protection MUST be set to 0.  An endpoint MUST treat
//#       receipt of a packet that has a non-zero value for these bits,
//#       after removing both packet and header protection, as a connection
//#       error of type PROTOCOL_VIOLATION.  Discarding such a packet after
//#       only removing header protection can expose the endpoint to attacks
//#       (see Section 9.3 of [QUIC-TLS]).
//#
//#    Key Phase (K):  The next bit (0x04) of byte 0 indicates the key
//#       phase, which allows a recipient of a packet to identify the packet
//#       protection keys that are used to protect the packet.  See
//#       [QUIC-TLS] for details.  This bit is protected using header
//#       protection (see Section 5.4 of [QUIC-TLS]).

const KEY_PHASE_MASK: u8 = 0x04;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProtectedKeyPhase;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyPhase {
    Zero,
    One,
}

impl KeyPhase {
    fn from_tag(tag: Tag) -> Self {
        if tag & KEY_PHASE_MASK == KEY_PHASE_MASK {
            Self::One
        } else {
            Self::Zero
        }
    }

    fn into_packet_tag_mask(self) -> u8 {
        match self {
            Self::One => KEY_PHASE_MASK,
            Self::Zero => 0,
        }
    }
}

//#    Packet Number Length (P):  The least significant two bits (those with
//#       a mask of 0x03) of byte 0 contain the length of the packet number,
//#       encoded as an unsigned, two-bit integer that is one less than the
//#       length of the packet number field in bytes.  That is, the length
//#       of the packet number field is the value of this field, plus one.
//#       These bits are protected using header protection (see Section 5.4
//#       of [QUIC-TLS]).
//#
//#    Destination Connection ID:  The Destination Connection ID is a
//#       connection ID that is chosen by the intended recipient of the
//#       packet.  See Section 5.1 for more details.
//#
//#    Packet Number:  The packet number field is 1 to 4 bytes long.  The
//#       packet number has confidentiality protection separate from packet
//#       protection, as described in Section 5.4 of [QUIC-TLS].  The length
//#       of the packet number field is encoded in Packet Number Length
//#       field.  See Section 17.1 for details.
//#
//#    Protected Payload:  Packets with a short header always include a
//#       1-RTT protected payload.
//#
//#    The header form bit and the connection ID field of a short header
//#    packet are version-independent.  The remaining fields are specific to
//#    the selected QUIC version.  See [QUIC-INVARIANTS] for details on how
//#    packets from different versions of QUIC are interpreted.

#[derive(Debug)]
pub struct Short<DCID, KeyPhase, PacketNumber, Payload> {
    pub spin_bit: SpinBit,
    pub key_phase: KeyPhase,
    pub destination_connection_id: DCID,
    pub packet_number: PacketNumber,
    pub payload: Payload,
}

pub type ProtectedShort<'a> =
    Short<CheckedRange, ProtectedKeyPhase, ProtectedPacketNumber, ProtectedPayload<'a>>;
pub type EncryptedShort<'a> = Short<CheckedRange, KeyPhase, PacketNumber, EncryptedPayload<'a>>;
pub type CleartextShort<'a> = Short<&'a [u8], KeyPhase, PacketNumber, DecoderBufferMut<'a>>;

impl<'a> ProtectedShort<'a> {
    #[inline]
    pub(crate) fn decode<DCID: DestinationConnectionIDDecoder>(
        tag: Tag,
        buffer: DecoderBufferMut<'a>,
        destination_connection_id_decoder: DCID,
    ) -> DecoderBufferMutResult<'a, ProtectedShort<'a>> {
        let mut decoder = HeaderDecoder::new_short(&buffer);

        let spin_bit = SpinBit::from_tag(tag);
        let key_phase = ProtectedKeyPhase;

        let destination_connection_id = decoder
            .decode_short_destination_connection_id(&buffer, destination_connection_id_decoder)?;

        let (payload, packet_number, remaining) =
            decoder.finish_short()?.split_off_packet(buffer)?;

        let packet = Short {
            spin_bit,
            key_phase,
            destination_connection_id,
            packet_number,
            payload,
        };

        Ok((packet, remaining))
    }

    pub fn unprotect<C: OneRTTCrypto>(
        self,
        crypto: &C,
        largest_acknowledged_packet_number: PacketNumber,
    ) -> Result<EncryptedShort<'a>, CryptoError> {
        let Short {
            spin_bit,
            destination_connection_id,
            payload,
            ..
        } = self;

        let (truncated_packet_number, payload) =
            crate::crypto::unprotect(crypto, PacketNumberSpace::ApplicationData, payload)?;

        let key_phase = KeyPhase::from_tag(payload.get_tag());

        let packet_number = truncated_packet_number
            .expand(largest_acknowledged_packet_number)
            .ok_or_else(CryptoError::decode_error)?;

        Ok(Short {
            spin_bit,
            key_phase,
            destination_connection_id,
            packet_number,
            payload,
        })
    }

    #[inline]
    pub fn destination_connection_id(&self) -> &[u8] {
        self.payload
            .get_checked_range(&self.destination_connection_id)
            .into_less_safe_slice()
    }
}

impl<'a> EncryptedShort<'a> {
    pub fn decrypt<C: OneRTTCrypto>(self, crypto: &C) -> Result<CleartextShort<'a>, CryptoError> {
        let Short {
            spin_bit,
            key_phase,
            destination_connection_id,
            packet_number,
            payload,
        } = self;

        let (header, payload) = crate::crypto::decrypt(crypto, packet_number, payload)?;

        let header = header.into_less_safe_slice();

        let destination_connection_id = destination_connection_id.get(header);

        Ok(Short {
            spin_bit,
            key_phase,
            destination_connection_id,
            packet_number,
            payload,
        })
    }

    #[inline]
    pub fn destination_connection_id(&self) -> &[u8] {
        self.payload
            .get_checked_range(&self.destination_connection_id)
            .into_less_safe_slice()
    }
}

impl<'a> CleartextShort<'a> {
    #[inline]
    pub fn destination_connection_id(&self) -> &[u8] {
        &self.destination_connection_id
    }
}

impl<DCID: EncoderValue, Payload: EncoderValue> EncoderValue
    for Short<DCID, KeyPhase, TruncatedPacketNumber, Payload>
{
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        self.encode_header(self.packet_number.len(), encoder);
        self.packet_number.encode(encoder);
        self.payload.encode(encoder);
    }
}

impl<DCID: EncoderValue, PacketNumber, Payload> Short<DCID, KeyPhase, PacketNumber, Payload> {
    fn encode_header<E: Encoder>(&self, packet_number_len: PacketNumberLen, encoder: &mut E) {
        (ENCODING_TAG
            | self.spin_bit.into_packet_tag_mask()
            | self.key_phase.into_packet_tag_mask()
            | packet_number_len.into_packet_tag_mask())
        .encode(encoder);

        self.destination_connection_id.encode(encoder);
    }
}

impl<DCID: EncoderValue, Payload: PacketPayloadEncoder, Crypto: OneRTTCrypto + HeaderCrypto>
    PacketEncoder<Crypto, Payload> for Short<DCID, KeyPhase, PacketNumber, Payload>
{
    type PayloadLenCursor = ();

    fn packet_number(&self) -> PacketNumber {
        self.packet_number
    }

    fn encode_header<E: Encoder>(&self, packet_number_len: PacketNumberLen, encoder: &mut E) {
        Short::encode_header(self, packet_number_len, encoder);
    }

    fn payload(&mut self) -> &mut Payload {
        &mut self.payload
    }
}