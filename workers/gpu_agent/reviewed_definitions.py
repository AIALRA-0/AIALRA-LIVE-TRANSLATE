"""Small, source-backed corrections for repeatedly misdefined technical concepts.

This is explanatory background, never an ASR prompt or an altered transcript.
Match an inventoried source term and its domain; unknown meanings still use the
model. References document the review basis, not fabricated lecturer citations.
"""

from __future__ import annotations

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class Definition:
    term: str
    explanation: str
    reference: str


PIPELINE_REFERENCE = (
    "https://ocw.mit.edu/courses/6-004-computation-structures-spring-2017/"
    "e8d0d9b8305f1e03aa58c21e91260438_6XV3uLfKzog.pdf"
)
ACTIVITY_REFERENCE = (
    "https://docs.amd.com/r/en-US/ug835-vivado-tcl-commands/set_switching_activity"
)
FREQUENCY_REFERENCE = "https://www.nist.gov/pml/owm/si-units-time"
STORAGE_REFERENCE = "https://docs.amd.com/r/en-US/ug574-ultrascale-clb/Storage-Elements"
ADC_REFERENCE = (
    "https://developerhelp.microchip.com/xwiki/bin/view/products/data-converters/"
    "adc-specs/adc-ac-specifications/quantization-error/"
)
CRC_REFERENCE = "https://www.rfc-editor.org/rfc/rfc3385"
PARTITION_REFERENCE = "https://limsk.ece.gatech.edu/book/papers/fm.pdf"
NETLIST_REFERENCE = "https://docs.amd.com/r/en-US/ug893-vivado-ide/Using-the-Netlist-Window"


def reviewed_definition(
    term: str, source: str, language: str, context: list[str] | None = None,
) -> Definition | None:
    if not language.casefold().startswith("zh") or not term.strip():
        return None
    left = r"(?<![A-Za-z0-9_])" if term[0].isascii() and term[0].isalnum() else ""
    right = r"(?![A-Za-z0-9_])" if term[-1].isascii() and term[-1].isalnum() else ""
    if re.search(left + re.escape(term) + right, source, re.I) is None:
        return None
    key = " ".join(term.casefold().split()).replace("–", "-").replace("‑", "-")
    # The term must occur in the current source; neighbouring source may only
    # establish its field, never add a term or become a fabricated citation.
    domain = "\n".join([source, *(context or [])])
    pipeline = re.search(r"\b(load|pipeline|instruction|register|operand|bypass)\b", domain, re.I)
    circuit = re.search(r"\b(clock|circuit|voltage|logic|switching|timing)\b", domain, re.I)
    partition = re.search(r"\b(partition|partitioning|cutsize|cut.size|hypergraph)\b", domain, re.I)
    if circuit and key in {"partition", "partitioning", "circuit partitioning"}:
        return Definition(
            "电路划分（Circuit Partitioning）",
            "电路划分是把电路中的单元分配到若干分区的过程；"
            "它用于组织设计并控制跨区连接、规模或其他设计约束；"
            "通常把单元及其连接表示成图或超图，再按选定代价和约束调整单元归属；"
            "划分只是设计中的一个步骤，不等于完成布局布线，也没有通用于所有电路的最佳分区数",
            PARTITION_REFERENCE,
        )
    if circuit and key in {"gate", "gates", "logic gate", "logic gates"}:
        return Definition(
            "逻辑门（Logic Gate）",
            "逻辑门是按照逻辑关系把输入组合转换为输出的电路单元，例如与门或非门；"
            "多个逻辑门通过连接组成更大的数字功能；"
            "电路描述中的门数量可用于描述设计规模，但必须说明采用的单元或等效门计数方法；"
            "门数量不是线网数量，不能根据两者在某个例子中接近，就假定所有电路都满足这一关系",
            NETLIST_REFERENCE,
        )
    if circuit and key in {"net", "nets", "signal net"}:
        return Definition(
            "线网（Net）",
            "线网是电路描述中把相关端口或引脚连接起来的逻辑连接对象；"
            "它说明哪些单元之间需要传递信号，供设计分析、划分和布线使用；"
            "一条线网可以连接多个引脚，实际物理导线则由后续实现确定；"
            "线网不是逻辑门，它的数量不能直接说明传播延迟，也不必与门数量相等",
            NETLIST_REFERENCE,
        )
    if partition and key in {
        "fm", "fm algorithm", "fiduccia-mattheyses", "fiduccia-mattheyses algorithm",
    }:
        return Definition(
            "FM 电路划分算法（Fiduccia–Mattheyses Algorithm）",
            "这是一种通过反复调整单元归属来改进电路分区的启发式算法；"
            "它在保持分区规模约束的同时，尝试减少跨区连接的割代价；"
            "一轮内逐个移动单元、更新收益并暂时锁定已移动单元，再保留累计收益最好的移动前缀；"
            "它适用于电路划分，不保证找到全局最优，也不表示调频或完全匹配",
            PARTITION_REFERENCE,
        )
    if partition and key in {"cut size", "cutsize", "cut cost", "net cut"}:
        return Definition(
            "割大小（Cut Size）",
            "割大小是衡量分区之间连接代价的指标；"
            "划分算法用它比较不同分区方案，通常希望减少跨区连接；"
            "普通图可以统计跨区边，电路超图通常统计跨区线网，带权目标则累加相应权重；"
            "比较数值前必须说明计数对象、权重和跨多个分区的计费方式，它不是分区面积",
            PARTITION_REFERENCE,
        )
    if circuit and key in {"latch", "latches", "level-sensitive latch", "level sensitive latch"}:
        return Definition(
            "电平敏感锁存器（Level-Sensitive Latch）",
            "电平敏感锁存器是保存一位状态的电路；"
            "使能处于有效电平期间，输出可以随输入变化，使能结束后保留最后状态；"
            "它用于需要受控接收和保持数据的时序电路，具体有效电平由设计决定；"
            "它与只在指定边沿采样的触发器不同，不能把某个器件的高有效或低有效推广到全部锁存器",
            STORAGE_REFERENCE,
        )
    if circuit and key in {"flip-flop", "flip-flops", "flip flop", "edge-triggered flip-flop"}:
        return Definition(
            "边沿触发器（Edge-Triggered Flip-Flop）",
            "边沿触发器是按指定时钟边沿采样并保持状态的存储电路；"
            "它使寄存器或流水线可以按离散时刻更新数据；"
            "输入在有效边沿附近满足相应时序条件后，输出仍需经过实际传播延迟才更新；"
            "上升沿还是下降沿由器件与设计决定，它不像锁存器那样在整个有效电平期间保持透明",
            STORAGE_REFERENCE,
        )
    if key in {"crc", "cyclic redundancy check"} and re.search(
        r"\b(data|check|checksum|error|packet|transmission|polynomial)\b", domain, re.I,
    ):
        return Definition(
            "CRC 循环冗余校验（Cyclic Redundancy Check）",
            "循环冗余校验是用于检测数据差错的校验方法；"
            "发送端按照约定多项式计算校验值，接收端按相同规则检查数据与校验值是否一致；"
            "它常用于传输和存储，检错能力取决于多项式、数据长度及错误模式；"
            "校验一致仍可能存在未检出的错误，它不等于纠错，也不能证明数据未被恶意篡改",
            CRC_REFERENCE,
        )
    if key in {"quantization error", "quantisation error"} and re.search(
        r"\b(adc|converter|analog|analogue|lsb|voltage)\b", domain, re.I,
    ):
        return Definition(
            "量化误差（Quantization Error）",
            "量化误差是连续输入与所选离散表示值之间的差；"
            "它描述有限分辨率对数值表示造成的偏差，大小由量化步长和取整规则决定；"
            "在不饱和且采用最近值舍入的理想模型中，误差幅度不超过半个量化步长，输入恰好可表示时可以为零；"
            "截断或饱和场景不能直接套用该界限，量化误差也不同于实际电路的失调、噪声和非线性误差",
            ADC_REFERENCE,
        )
    if pipeline and key in {
        "load-use", "load-to-use", "load-use hazard", "load-to-use hazard",
        "load-use dependency", "load-use dependencies",
    }:
        return Definition(
            "加载后使用依赖（load-use dependency）",
            "加载后使用依赖是后续指令需要使用先前数据加载结果的关系，"
            "当结果尚未可供使用时，它会形成流水线数据冒险，需要等待或利用旁路传递结果，"
            "具体等待多久取决于数据何时到达以及哪一级需要它，不等于必须等到写回寄存器，"
            "这里的加载是读取数据，不是取出下一条指令",
            PIPELINE_REFERENCE,
        )
    if pipeline and key in {"register tag", "register tags"}:
        return Definition(
            "寄存器标识（register tag）",
            "寄存器标识是在相关处理器设计中用于辨认操作数或结果归属的标签，"
            "控制逻辑通过比较标识判断某条指令是否依赖另一个结果，"
            "标识本身不是数据，是否有效、结果是否就绪还需由相应状态判断，"
            "具体编码以及它代表架构寄存器还是其他内部对象，取决于处理器设计",
            PIPELINE_REFERENCE,
        )
    if circuit and key in {"activity factor", "switching activity factor"}:
        return Definition(
            "活动因子（activity factor）",
            "活动因子是相对于参考时钟周期描述信号切换活跃程度的无量纲量，"
            "功耗估算用它区分经常切换和很少切换的节点，它不是以赫兹计量的频率，"
            "使用时必须说明统计的是充电事件还是双向翻转，不同公式和工具的计数约定不能直接混用",
            ACTIVITY_REFERENCE,
        )
    if key in {"frequency", "clock frequency", "mhz", "hz", "megahertz"} and (
        circuit or key in {"mhz", "hz", "megahertz"}
    ):
        return Definition(
            "兆赫（MHz）" if key in {"mhz", "megahertz"} else "频率（frequency）",
            "频率描述周期性变化每秒重复多少个完整周期，单位赫兹表示每秒一个周期，"
            "一兆赫等于一百万赫兹，时钟的一次上升或下降不是各算一个完整周期，"
            "频率只描述变化节奏，不能单独决定系统吞吐量或可以并行工作的部件数量",
            FREQUENCY_REFERENCE,
        )
    return None
