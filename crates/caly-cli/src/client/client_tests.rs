use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use caly_protocol::protocol::v2::WireMode;

    #[test]
    fn parses_32_hex_node_id() {
        let id = execute::ops::parse_node_id("00000000000000000000000000000001");
        let mut expected = [0_u8; 16];
        expected[15] = 1;
        assert_eq!(id, Some(expected));
    }

    #[test]
    fn rejects_bad_node_id_length_and_chars() {
        assert_eq!(execute::ops::parse_node_id("short"), None);
        assert_eq!(
            execute::ops::parse_node_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            None
        );
        assert_eq!(
            execute::ops::parse_node_id("0000000000000000000000000000000"),
            None
        ); // 31 chars
    }

    #[test]
    fn routing_modes_parse_through_the_wire_layer() {
        assert_eq!(WireMode::from_label("rule").map(WireMode::wire), Some(1));
        assert_eq!(WireMode::from_label("global").map(WireMode::wire), Some(2));
        assert_eq!(WireMode::from_label("direct").map(WireMode::wire), Some(3));
        assert_eq!(WireMode::from_label("nope"), None);
    }
}
