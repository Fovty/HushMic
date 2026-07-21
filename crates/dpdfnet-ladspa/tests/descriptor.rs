use dpdfnet_ladspa::get_ladspa_descriptor;

#[test]
fn descriptor_has_expected_shape() {
    let d = get_ladspa_descriptor(0).expect("descriptor 0");
    assert_eq!(d.label, "dpdfnet_mono");
    // 2 audio (in,out) + 2 controls (attn, mode) = 4 ports
    assert_eq!(d.ports.len(), 4);
    assert!(get_ladspa_descriptor(1).is_none(), "only one plugin");
}
