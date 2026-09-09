"""Writing changes cannot rewrite protected source text or factual values."""

from workers.model_worker.teaching_format import bilingual_term, generated_prose


def test_generated_punctuation_preserves_quotes_code_math_and_values() -> None:
    assert generated_prose("电压是 1.25 V。阈值为 0.8 V。\n不改变条件；") == (
        "电压是 1.25 V；阈值为 0.8 V\n不改变条件"
    )
    protected = '“原话。” `code。` $a=1.25$ 「引用。」 "引用。"\n```\n代码。\n```'
    assert generated_prose(protected) == protected
    assert generated_prose('第一项。“原话。”') == '第一项；“原话。”'
    assert generated_prose("不能保证成功。\n\n只有条件满足才成立。") == (
        "不能保证成功\n\n只有条件满足才成立"
    )


def test_bilingual_names_only_reorder_existing_names() -> None:
    assert bilingual_term("Register (寄存器)") == "寄存器（Register）"
    assert bilingual_term("寄存器 (Register)") == "寄存器（Register）"
    assert bilingual_term("FM 电路划分算法（Fiduccia–Mattheyses Algorithm）") == (
        "FM 电路划分算法（Fiduccia–Mattheyses Algorithm）"
    )
    assert bilingual_term("未知名词") == "未知名词"
    assert bilingual_term("New Thing") == "New Thing"
    assert bilingual_term("模型（版本 2）") == "模型（版本 2）"
