"""Reference-backed definitions must not leak across domains or languages."""

from workers.gpu_agent.reviewed_definitions import reviewed_definition


def test_known_factual_errors_have_reviewed_background() -> None:
    cases = [
        ("load-use", "A load-use dependency may stall this pipeline.", "不是取出"),
        ("activity factor", "The circuit activity factor affects power.", "无量纲"),
        ("MHz", "This clock runs at 20 MHz.", "完整周期"),
        ("register tags", "Compare the register tags of these operands.", "不是数据"),
    ]
    for term, source, fact in cases:
        definition = reviewed_definition(term, source, "zh-CN")
        assert definition and fact in definition.explanation
        assert definition.reference.startswith("https://")


def test_missing_unknown_ambiguous_and_other_language_terms_still_use_model() -> None:
    unrelated = "activity factor in daily exercise"
    assert reviewed_definition("activity factor", unrelated, "zh-CN") is None
    assert reviewed_definition("frequency", "frequency of bus visits", "zh-CN") is None
    assert reviewed_definition("load-use", "The pipeline stalls.", "zh-CN") is None
    assert reviewed_definition("MHz", "Clock MHz", "en") is None
    assert reviewed_definition("quantization error", "Abstract quantization error", "zh-CN") is None


def test_new_reviewed_background_keeps_conditions_and_does_not_guess_domains() -> None:
    cases = [
        ("FM", "FM improves hypergraph partitioning", "不保证"),
        ("cut size", "Compare partitioning cut size", "跨区线网"),
        ("latch", "The circuit uses a latch", "有效电平由设计决定"),
        ("flip-flop", "The circuit flip-flop stores it", "实际传播延迟"),
        ("CRC", "CRC checks data errors", "未检出的错误"),
        ("quantization error", "ADC quantization error", "可以为零"),
        ("nets", "The circuit has nets", "不必与门数量相等"),
        ("gates", "The circuit has gates", "不是线网数量"),
        ("partition", "A circuit partition", "不等于完成布局布线"),
    ]
    for term, source, condition in cases:
        definition = reviewed_definition(term, source, "zh-CN")
        assert definition is not None and condition in definition.explanation
        assert definition.explanation.count("；") >= 3
        assert "。" not in definition.explanation
    assert reviewed_definition("FM", "Tune the FM radio", "zh-CN") is None
    assert reviewed_definition("latch", "The door latch is stuck", "zh-CN") is None
    assert reviewed_definition("FM", "FM improves the result", "zh-CN",
                               ["The previous paragraph discusses hypergraph partitioning"])
    assert reviewed_definition("FM", "The result improves", "zh-CN", ["FM partitioning"]) is None
    assert reviewed_definition("net", "Read the circuit internet guide", "zh-CN") is None
