use std::net::Ipv4Addr;

use anyhow::{anyhow, Result};

use super::protocol::Protocol;
use crate::edebug;

#[derive(Debug)]
pub struct IpPacket {
  /// Contains all of the mandatory header information
  header:      [u8; 20],
  /// Option data must have len of a multiple of 4
  option_data: Vec<u8>,
  /// Actual data associated with packet
  data:        Vec<u8>,
  /// Source address set
  source_set:  bool,
}

impl IpPacket {
  /// Note that most getters and setters do not bounds check, so at a minimum
  /// new must make header the correct length
  ///
  /// Note that source address isn't set in the constructor, as the IP layer sets the source when
  /// sending
  pub fn new_with_defaults(
    destination: Ipv4Addr,
    protocol: Protocol,
    data: &[u8],
  ) -> Result<IpPacket> {
    IpPacket::new(destination, protocol, 0u8, 255u8, data, 0u16, true, &[])
  }

  pub fn new(
    destination: Ipv4Addr,
    protocol: Protocol,
    type_of_service: u8,
    time_to_live: u8,
    data: &[u8],
    identifier: u16,
    dont_fragment: bool,
    option_data: &[u8],
  ) -> Result<IpPacket> {
    let mut packet = IpPacket {
      header:      [0u8; 20],
      option_data: option_data.to_vec(),
      data:        data.to_vec(),
      source_set:  false,
    };

    packet.set_version(4)?;
    let mut option_len = u8::try_from(option_data.len())?;
    while option_len % 4 != 0 {
      packet.option_data.push(0u8);
      option_len += 1;
    }
    let optional_length_in_words = option_len / 4u8;
    packet.set_internet_header_length(5u8 + optional_length_in_words)?;

    let data_len: u16 = u16::try_from(data.len())?;
    packet.set_total_length(20u16 + u16::from(option_len) + data_len);
    packet.set_identification(identifier);
    // Set flags so that our packets don't fragment
    packet.set_fragment_offset(FragmentOffset::new(dont_fragment, false, 0)?);
    packet.set_time_to_live(time_to_live);
    packet.set_protocol(protocol);
    packet.set_destination_address(destination);
    packet.set_type_of_service(type_of_service);

    packet.calculate_and_set_checksum();

    Ok(packet)
  }

  /// Unpacks a byte stream into an IpPacket, returns an error if the packet is
  /// malformed.
  pub(crate) fn unpack(bytes: &[u8]) -> Result<IpPacket> {
    let bytes_len = bytes.len();
    if bytes_len < 20 {
      return Err(anyhow!(
        "ip packet must be >= 20 bytes, len was {bytes_len}"
      ));
    }

    let mut packet = IpPacket {
      header:      [0u8; 20],
      option_data: Vec::new(),
      data:        Vec::new(),
      source_set:  true,
    };

    packet.header.copy_from_slice(&bytes[0..20]);

    let header_length_words = usize::from(packet.internet_header_length());
    let header_length_bytes = header_length_words * 4;

    if bytes_len < header_length_bytes {
      return Err(anyhow!(
        "header length is {header_length_bytes}, but bytes_len is {bytes_len}"
      ));
    }

    packet.option_data = bytes[20..header_length_bytes].to_vec();

    let total_length = usize::from(packet.total_length());
    if bytes_len < total_length {
      return Err(anyhow!(
        "Total length is {total_length}, but bytes_len is {bytes_len}"
      ));
    }

    packet.data = bytes[header_length_bytes..total_length].to_vec();

    packet.validate_header()?;

    return Ok(packet);
  }

  /// Returns Ok(()) if the header is well formed, otherwise errors
  fn validate_header(&self) -> Result<()> {
    self.validate_checksum()?;
    self.validate_options()?;

    Ok(())
  }

  /// Turn packet in bytes to be sent over network
  pub(crate) fn pack(&self) -> Vec<u8> {
    if !self.source_set {
      panic!(
        "{:#?} doesn't have source set. This is an implementation bug.",
        self
      );
    }
    let mut res = self.header.to_vec();
    debug_assert!(self.option_data.len() % 4 == 0);
    res.extend(&self.option_data);
    res.extend(&self.data);
    return res;
  }

  pub fn header(&self) -> &[u8] {
    &self.header
  }

  pub fn data(&self) -> &[u8] {
    &self.data
  }

  pub fn version(&self) -> u8 {
    get_high_order_four_bits(&self.header[0])
  }

  pub fn internet_header_length(&self) -> u8 {
    get_low_order_four_bits(&self.header[0])
  }

  pub fn type_of_service(&self) -> u8 {
    self.header[1]
  }

  pub fn total_length(&self) -> u16 {
    convert_to_u16(&self.header[2], &self.header[3])
  }

  pub fn identification(&self) -> u16 {
    convert_to_u16(&self.header[4], &self.header[5])
  }

  pub fn fragment_offset(&self) -> FragmentOffset {
    // unwrap because we assume that once a IpPacket is formed it is valid
    FragmentOffset::unpack(&self.header[6], &self.header[7]).unwrap()
  }

  pub fn time_to_live(&self) -> u8 {
    self.header[8]
  }

  pub fn protocol(&self) -> Protocol {
    // unwrap because we assume that once a IpPacket is formed it is valid
    Protocol::try_from(self.header[9]).unwrap()
  }

  pub fn header_checksum(&self) -> u16 {
    convert_to_u16(&self.header[10], &self.header[11])
  }

  pub fn source_address(&self) -> Ipv4Addr {
    if !self.source_set {
      edebug!("warning: {:#?} has no source set", self);
    }
    Ipv4Addr::new(
      self.header[12],
      self.header[13],
      self.header[14],
      self.header[15],
    )
  }

  pub fn destination_address(&self) -> Ipv4Addr {
    Ipv4Addr::new(
      self.header[16],
      self.header[17],
      self.header[18],
      self.header[19],
    )
  }

  /// Performs full checksum calculation
  fn calculate_checksum(&self) -> u16 {
    let mut checksum = 0u16;
    for i in (0..20).step_by(2) {
      // skip checksum field
      if i != 10 {
        let (s, c) = checksum.overflowing_add(convert_to_u16(&self.header[i], &self.header[i + 1]));
        checksum = if c { s + 1 } else { s };
      }
    }

    debug_assert!(self.option_data.len() % 4 == 0);
    for i in (0..self.option_data.len()).step_by(2) {
      let (s, c) = checksum.overflowing_add(convert_to_u16(
        &self.option_data[i],
        &self.option_data[i + 1],
      ));
      checksum = if c { s + 1 } else { s };
    }

    // Note ! is bitwise not
    !checksum
  }

  fn validate_checksum(&self) -> Result<()> {
    let expected = self.calculate_checksum();
    let actual = self.header_checksum();
    if expected == actual {
      Ok(())
    } else {
      Err(anyhow!(
        "{:#?}\nChecksum invalid, expected {expected}, actual {actual}",
        self
      ))
    }
  }

  fn calculate_and_set_checksum(&mut self) {
    self.set_header_checksum(self.calculate_checksum());
  }

  /// Warning, checksum should always be set by other setters or in the
  /// initialize_checksum function
  pub(crate) fn set_header_checksum(&mut self, checksum: u16) {
    self.header[10] = get_high_order_byte(&checksum);
    self.header[11] = get_low_order_byte(&checksum);
  }

  /// Sets ip version number
  /// i.e. 4
  pub(crate) fn set_version(&mut self, version: u8) -> Result<()> {
    if version >= 1 << 4 {
      return Err(anyhow!("Version only gets 4 bits but was set to {version}"));
    }
    set_high_order_four_bits(&mut self.header[0], &version);

    self.calculate_and_set_checksum();
    Ok(())
  }

  pub(crate) fn set_internet_header_length(&mut self, ihl: u8) -> Result<()> {
    if ihl >= 1 << 4 {
      return Err(anyhow!("IHL only gets 4 bits but was set to {ihl}"));
    }
    set_low_order_four_bits(&mut self.header[0], &ihl);

    self.calculate_and_set_checksum();
    Ok(())
  }

  /// Sets the first byte of ip packet
  /// i.e. 4
  pub(crate) fn _set_version_and_ihl(&mut self, byte: u8) {
    self.header[0] = byte;
  }

  pub(crate) fn set_type_of_service(&mut self, type_of_service: u8) {
    self.header[1] = type_of_service;
  }

  pub(crate) fn set_total_length(&mut self, total_length: u16) {
    self.header[2] = get_high_order_byte(&total_length);
    self.header[3] = get_low_order_byte(&total_length);

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_identification(&mut self, identification: u16) {
    self.header[4] = get_high_order_byte(&identification);
    self.header[5] = get_low_order_byte(&identification);

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_fragment_offset(&mut self, fragment_offset: FragmentOffset) {
    (self.header[6], self.header[7]) = fragment_offset.to_bytes();

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_time_to_live(&mut self, time_to_live: u8) {
    self.header[8] = time_to_live;

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_protocol(&mut self, protocol: Protocol) {
    self.header[9] = protocol.into();

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_source_address(&mut self, source_address: Ipv4Addr) {
    self.source_set = true;
    let addr_bytes = source_address.octets();
    self.header[12] = addr_bytes[0];
    self.header[13] = addr_bytes[1];
    self.header[14] = addr_bytes[2];
    self.header[15] = addr_bytes[3];

    self.calculate_and_set_checksum();
  }

  pub(crate) fn set_destination_address(&mut self, destination_address: Ipv4Addr) {
    let addr_bytes = destination_address.octets();
    self.header[16] = addr_bytes[0];
    self.header[17] = addr_bytes[1];
    self.header[18] = addr_bytes[2];
    self.header[19] = addr_bytes[3];

    self.calculate_and_set_checksum();
  }

  /// sets the payload of the ip packet
  pub(crate) fn _set_data(&mut self, data: Vec<u8>) {
    self.data = data;
  }

  fn validate_options(&self) -> Result<()> {
    if self.option_data.len() % 4 != 0 {
      return Err(anyhow!(
        "option_data should be padded so that len is multiple of 4"
      ));
    }

    Ok(())
  }

  pub(crate) fn source_address_set(&self) -> bool {
    self.source_set
  }
}

#[derive(Debug, PartialEq)]
pub struct FragmentOffset {
  /// This is the second bit in the flags section of the IP header
  pub(crate) dont_fragment:  bool,
  /// This is the third bit in the flags section of the IP header
  pub(crate) more_fragments: bool,

  /// really a u13
  fragment_offset: u16,
}

impl FragmentOffset {
  const MAX_FRAGMENT_OFFSET: u16 = (1 << 13) - 1;

  /// Creates a new FragmentOffset object and bound checks fragment_offset
  pub(crate) fn new(
    dont_fragment: bool,
    more_fragments: bool,
    fragment_offset: u16,
  ) -> Result<FragmentOffset> {
    if fragment_offset > FragmentOffset::MAX_FRAGMENT_OFFSET {
      return Err(anyhow!(
        "fragment_offset has max value of {}, value was {}",
        FragmentOffset::MAX_FRAGMENT_OFFSET,
        fragment_offset,
      ));
    }

    Ok(FragmentOffset {
      dont_fragment,
      more_fragments,
      fragment_offset,
    })
  }

  /// Takes in the bytes from a ip packet and forms a fragment offset
  pub(crate) fn unpack(high_order_byte: &u8, low_order_byte: &u8) -> Result<FragmentOffset> {
    if *high_order_byte & 0b1000_0000u8 != 0 {
      return Err(anyhow!(
        "First bit of fragment flags is reserved and must be 0"
      ));
    }
    let dont_fragment = bool::from(*high_order_byte & 0b0100_0000u8 != 0);
    let more_fragments = bool::from(*high_order_byte & 0b0010_0000u8 != 0);
    let high_order_offset_byte = *high_order_byte & 0b0001_1111u8;
    let fragment_offset = convert_to_u16(&high_order_offset_byte, low_order_byte);

    Ok(FragmentOffset {
      dont_fragment,
      more_fragments,
      fragment_offset,
    })
  }

  /// Returns (high_order_byte, low_order_byte)
  pub(crate) fn to_bytes(&self) -> (u8, u8) {
    let mut high_order_byte = 0b0000_0000u8;
    if self.dont_fragment {
      high_order_byte |= 0b0100_0000;
    }

    if self.more_fragments {
      high_order_byte |= 0b0010_0000;
    }

    debug_assert!(self.fragment_offset <= FragmentOffset::MAX_FRAGMENT_OFFSET);
    // use += instead of |= to avoid explicit conversion
    high_order_byte += get_high_order_byte(&self.fragment_offset);
    let low_order_byte = get_low_order_byte(&self.fragment_offset);

    (high_order_byte, low_order_byte)
  }

  /// Sets fragment offset value with bounds checking
  fn _set_fragment_offset(&mut self, fragment_offset: u16) -> Result<()> {
    if fragment_offset > FragmentOffset::MAX_FRAGMENT_OFFSET {
      return Err(anyhow!(
        "fragment_offset has max value of {}, value was {}",
        FragmentOffset::MAX_FRAGMENT_OFFSET,
        fragment_offset,
      ));
    }

    self.fragment_offset = fragment_offset;
    Ok(())
  }

  fn _fragment_offset(&self) -> u16 {
    self.fragment_offset
  }
}

fn get_high_order_four_bits(byte: &u8) -> u8 {
  byte >> 4
}

fn get_low_order_four_bits(byte: &u8) -> u8 {
  byte & 0b0000_1111
}

fn set_high_order_four_bits(byte: &mut u8, high_bits: &u8) {
  debug_assert!(*high_bits < (1 << 4));
  *byte &= 0b0000_1111;
  *byte |= high_bits << 4;
}

fn set_low_order_four_bits(byte: &mut u8, low_bits: &u8) {
  debug_assert!(*low_bits < (1 << 4));
  *byte &= 0b1111_0000;
  *byte |= low_bits;
}

fn get_high_order_byte(num: &u16) -> u8 {
  (num >> 8) as u8
}

fn get_low_order_byte(num: &u16) -> u8 {
  (num & 0xffu16) as u8
}

fn convert_to_u16(higher_order: &u8, lower_order: &u8) -> u16 {
  (u16::from(*higher_order) << 8) + u16::from(*lower_order)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_get_high_order_four_bits() {
    let byte = 0b1111_0000u8;
    assert_eq!(get_high_order_four_bits(&byte), 0b0000_1111u8);
    let byte = 0b1111_1101u8;
    assert_eq!(get_high_order_four_bits(&byte), 0b0000_1111u8);
    let byte = 0b0110_0000u8;
    assert_eq!(get_high_order_four_bits(&byte), 0b0000_0110u8);
    let byte = 0b0110_1001u8;
    assert_eq!(get_high_order_four_bits(&byte), 0b0000_0110u8);
  }

  #[test]
  fn test_get_low_order_four_bits() {
    let byte = 0b1111_0000u8;
    assert_eq!(get_low_order_four_bits(&byte), 0b0000_0000u8);
    let byte = 0b0110_0110u8;
    assert_eq!(get_low_order_four_bits(&byte), 0b0000_0110u8);
    let byte = 0b0110_1001u8;
    assert_eq!(get_low_order_four_bits(&byte), 0b0000_1001u8);
  }

  #[test]
  fn test_set_high_order_four_bits() {
    let mut byte = 0b0000_0000u8;
    let high_bits = 0b0000_1111u8;
    set_high_order_four_bits(&mut byte, &high_bits);
    assert_eq!(byte, 0b1111_0000);

    let mut byte = 0b1111_1111u8;
    let high_bits = 0b0000_0000u8;
    set_high_order_four_bits(&mut byte, &high_bits);
    assert_eq!(byte, 0b0000_1111);

    let mut byte = 0b0110_1001u8;
    let high_bits = 0b0000_1010u8;
    set_high_order_four_bits(&mut byte, &high_bits);
    assert_eq!(byte, 0b1010_1001);
  }

  #[test]
  fn test_set_low_order_four_bits() {
    let mut byte = 0b0000_0000u8;
    let low_bits = 0b0000_1111u8;
    set_low_order_four_bits(&mut byte, &low_bits);
    assert_eq!(byte, 0b0000_1111);

    let mut byte = 0b1111_1111u8;
    let low_bits = 0b0000_0000u8;
    set_low_order_four_bits(&mut byte, &low_bits);
    assert_eq!(byte, 0b1111_0000);

    let mut byte = 0b0110_1001u8;
    let low_bits = 0b0000_1010u8;
    set_low_order_four_bits(&mut byte, &low_bits);
    assert_eq!(byte, 0b0110_1010);
  }

  #[test]
  fn test_get_high_order_byte() {
    let byte = 0x00ffu16;
    assert_eq!(get_high_order_byte(&byte), 0u8);
    let byte = 0xff00u16;
    assert_eq!(get_high_order_byte(&byte), 0xffu8);
    let byte = 0b0110_0000_1001_1010u16;
    assert_eq!(get_high_order_byte(&byte), 0b0110_0000u8);
  }

  #[test]
  fn test_get_low_order_byte() {
    let byte = 0x00ffu16;
    assert_eq!(get_low_order_byte(&byte), 0xffu8);
    let byte = 0xff00u16;
    assert_eq!(get_low_order_byte(&byte), 0u8);
    let byte = 0b0110_0000_1001_1010u16;
    assert_eq!(get_low_order_byte(&byte), 0b1001_1010u8);
  }

  #[test]
  fn test_convert_to_u16() {
    let low_order_byte = 0b0000_0000u8;
    let high_order_byte = 0b1111_1111u8;
    assert_eq!(
      convert_to_u16(&high_order_byte, &low_order_byte),
      0b1111_1111_0000_0000u16
    );
    assert_eq!(
      convert_to_u16(&low_order_byte, &high_order_byte),
      0b0000_0000_1111_1111u16
    );

    let low_order_byte = 0b0110_0101u8;
    let high_order_byte = 0b1010_1001u8;
    assert_eq!(
      convert_to_u16(&high_order_byte, &low_order_byte),
      0b1010_1001_0110_0101u16
    );
  }

  #[test]
  fn test_fragment_offset() {
    let high_order_byte = 0b0110_0000;
    let low_order_byte = 0b0000_0000;
    let fragment_offset = FragmentOffset::unpack(&high_order_byte, &low_order_byte).unwrap();
    let expected_fragment_offset = FragmentOffset::new(true, true, 0).unwrap();
    assert_eq!(fragment_offset, expected_fragment_offset);
    assert_eq!(
      fragment_offset.to_bytes(),
      (high_order_byte, low_order_byte)
    );

    let high_order_byte = 0b0111_0010;
    let low_order_byte = 0b0010_0100;
    let fragment_offset = FragmentOffset::unpack(&high_order_byte, &low_order_byte).unwrap();
    let expected_fragment_offset_value = 0b0001_0010_0010_0100;
    let expected_fragment_offset =
      FragmentOffset::new(true, true, expected_fragment_offset_value).unwrap();
    assert_eq!(fragment_offset, expected_fragment_offset);
    assert_eq!(
      fragment_offset.to_bytes(),
      (high_order_byte, low_order_byte)
    );

    let invalid_offset = FragmentOffset::MAX_FRAGMENT_OFFSET + 1;
    assert!(FragmentOffset::new(true, true, invalid_offset).is_err());
  }
}
