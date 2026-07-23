//! Tests for MultiLegAction (0.9.0 D1.4b).

use axon_inference::MultiLegAction;

#[test]
fn multi_leg_action_2_legs() {
    let action = MultiLegAction {
        target_positions: vec![0.5, -0.3],
        model_id: "test_model".to_string(),
        inference_time_us: 150,
    };
    assert_eq!(action.target_positions.len(), 2);
    assert_eq!(action.target_positions[0], 0.5);
    assert_eq!(action.target_positions[1], -0.3);
}

#[test]
fn multi_leg_action_serialize_roundtrip() {
    let action = MultiLegAction {
        target_positions: vec![1.0, -1.0, 0.0],
        model_id: "m".to_string(),
        inference_time_us: 100,
    };
    let json = serde_json::to_string(&action).unwrap();
    let deserialized: MultiLegAction = serde_json::from_str(&json).unwrap();
    assert_eq!(action, deserialized);
}
