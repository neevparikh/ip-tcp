use super::protocol::Protocol;

use std::net::Ipv4Addr;

use anyhow::{anyhow, Result};

fn get_high_order_four_bits(byte: &u8) -> u8 {
  byte >> 4
}

fn get_low_order_four_bits(byte: &u8) -> u8 {
  byte & 0b0000_1111
}

fn convert_to_u16(higher_order: &u8, lower_order: &u8) -> u16 {
  (u16::from(*higher_order) << 8) + u16::from(*lower_order)
}

#[derive(Debug, PartialEq)]
pub struct FragmentOffset {
  /// This is the second bit in the flags section of the IP header
  pub dont_fragment: bool,
  /// This is the third bit in the flags section of the IP header
  pub more_fragments: bool,

  /// really a u13
  fragment_offset: u16,
}

impl FragmentOffset {
  const MAX_FRAGMENT_OFFSET: u16 = (1 << 13) - 1;

  pub fn new(
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

  pub fn unpack(high_order_byte: &u8, low_order_byte: &u8) -> FragmentOffset {
    let dont_fragment = bool::from(*high_order_byte & 0b0100_0000u8 != 0);
    let more_fragments = bool::from(*high_order_byte & 0b0010_0000u8 != 0);
    let high_order_offset_byte = *high_order_byte & 0b0001_1111u8;
    let fragment_offset = convert_to_u16(&high_order_offset_byte, low_order_byte);

    FragmentOffset {
      dont_fragment,
      more_fragments,
      fragment_offset,
    }
  }

  /// Returns (high_order_byte, low_order_byte)
  pub fn to_bytes(&self) -> (u8, u8) {
    let mut high_order_byte = 0b0000_0000u8;
    if self.dont_fragment {
      high_order_byte |= 0b0100_0000;
    }

    if self.more_fragments {
      high_order_byte |= 0b0010_0000;
    }

    debug_assert!(self.fragment_offset <= FragmentOffset::MAX_FRAGMENT_OFFSET);
    // use += instead of |= to avoid explicit conversion
    high_order_byte += u8::try_from(self.fragment_offset >> 8).unwrap();
    let low_order_byte = u8::try_from(dbg!(self.fragment_offset & 0xffu16)).unwrap();

    (high_order_byte, low_order_byte)
  }

  fn set_fragment_offset(&mut self, fragment_offset: u16) -> Result<()> {
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

  fn get_fragment_offset(&self) -> u16 {
    self.fragment_offset
  }
}

pub struct IpPacket {
  header: Vec<u8>,
  data: Vec<u8>,
}

impl IpPacket {
  /// Note that most getters and setters do not bounds check, so at a minimum
  /// new must make header the correct length
  pub fn new() -> IpPacket {
    todo!();
  }

  pub fn unpack(bytes: &[u8]) -> IpPacket {
    todo!();
  }

  pub fn pack(&self) -> Vec<u8> {
    todo!();
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

  pub fn flags(&self) -> u16 {
    convert_to_u16(&self.header[4], &self.header[5])
  }

  pub fn protocol(&self) -> Result<Protocol> {
    todo!();
  }

  pub fn destination_addr(&self) -> Ipv4Addr {
    todo!();
  }
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
    let fragment_offset = FragmentOffset::unpack(&high_order_byte, &low_order_byte);
    let expected_fragment_offset = FragmentOffset::new(true, true, 0).unwrap();
    assert_eq!(fragment_offset, expected_fragment_offset);
    assert_eq!(
      fragment_offset.to_bytes(),
      (high_order_byte, low_order_byte)
    );

    let high_order_byte = 0b0111_0010;
    let low_order_byte = 0b0010_0100;
    let fragment_offset = FragmentOffset::unpack(&high_order_byte, &low_order_byte);
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
    let mut fragment_offset = FragmentOffset::new(true, true, 0).unwrap();
    assert!(fragment_offset.set_fragment_offset(invalid_offset).is_err());
  }
}
