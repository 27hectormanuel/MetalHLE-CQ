from app.log_parser import parse_log


def test_metalhle_log_header_and_driver_info_are_recognized():
    log = """MetalHLE 2.0 PREVIEW v1.0.35 (8d65eca)
MetalHLE::window: Driver info: OpenGL ES-CM 1.1 v1.r32p1 / ARM / Mali-G57 MC2
"""

    parsed = parse_log(log)

    assert parsed.emulator_version == "PREVIEW v1.0.35 (8d65eca)"
    assert parsed.gpu == "ARM Mali-G57 MC2"


def test_legacy_hyperhle_log_header_remains_supported():
    log = """HyperHLE v1.0.2 — https://touchhle.org/
HyperHLE::window: Driver info: OpenGL ES-CM 1.1 v1.r32p1 / ARM / Adreno 730
"""

    parsed = parse_log(log)

    assert parsed.emulator_version == "v1.0.2"
    assert parsed.gpu == "ARM Adreno 730"
