#!/usr/bin/env python3
"""Generates the office-diff corpus: a Word document, a slide deck and a
workbook, all as raw OOXML written by this script.

Licence: CC0-1.0. The content is original except two short public-domain
excerpts (Li Bai's 静夜思, Tang dynasty; the proverb "Slow and steady wins the
race"). Nothing here comes from a private document. See README.md.

Run: python3 tools/office-diff/corpus/generate.py --out /tmp/corpus
"""

import argparse
import os
import struct
import zipfile
import zlib

# ---------------------------------------------------------------- helpers


def esc(text):
    return (
        str(text)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def png_bytes(width, height, pixel):
    """A small RGB PNG from a pixel(x, y) -> (r, g, b) function."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixel(x, y))

    def chunk(kind, data):
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def sample_png():
    """Blue field with an orange disc: a picture whose colours are easy to see."""

    def pixel(x, y):
        dx, dy = x - 48, y - 32
        if dx * dx + dy * dy < 20 * 20:
            return (230, 126, 34)
        if 36 <= x < 60 and 20 <= y < 44:
            return (255, 255, 255)
        return (31 + x // 4, 78 + y // 3, 121 + y // 5)

    return png_bytes(96, 64, pixel)


W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
WP = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
PIC = "http://schemas.openxmlformats.org/drawingml/2006/picture"
PKG = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"


def rels(items, base=R):
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<Relationships xmlns="{PKG}">'
        + "".join(
            f'<Relationship Id="{i}" Type="{base}/{kind}" Target="{esc(target)}"/>'
            for i, kind, target in items
        )
        + "</Relationships>"
    )


# ---------------------------------------------------------------- Word

WORD_STYLES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W}">
 <w:docDefaults>
  <w:rPrDefault><w:rPr>
   <w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="宋体" w:cs="Calibri"/>
   <w:sz w:val="22"/><w:szCs w:val="22"/>
   <w:lang w:val="en-US" w:eastAsia="zh-CN"/>
  </w:rPr></w:rPrDefault>
  <w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault>
 </w:docDefaults>
 <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>
 <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/>
  <w:pPr><w:jc w:val="center"/><w:spacing w:before="240" w:after="240"/></w:pPr>
  <w:rPr><w:rFonts w:ascii="Calibri Light" w:eastAsia="黑体"/><w:b/><w:sz w:val="44"/></w:rPr></w:style>
 <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/>
  <w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr>
  <w:rPr><w:rFonts w:ascii="Calibri" w:eastAsia="黑体"/><w:b/><w:sz w:val="32"/><w:color w:val="1F4E79"/></w:rPr></w:style>
 <w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/>
  <w:pPr><w:keepNext/><w:spacing w:before="200" w:after="80"/><w:outlineLvl w:val="1"/></w:pPr>
  <w:rPr><w:b/><w:sz w:val="28"/><w:color w:val="2E74B5"/></w:rPr></w:style>
 <w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:basedOn w:val="Normal"/>
  <w:pPr><w:ind w:left="720"/><w:spacing w:before="120" w:after="120"/></w:pPr>
  <w:rPr><w:i/><w:color w:val="595959"/></w:rPr></w:style>
 <w:style w:type="paragraph" w:styleId="FootnoteText"><w:name w:val="footnote text"/><w:basedOn w:val="Normal"/>
  <w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="18"/></w:rPr></w:style>
 <w:style w:type="character" w:styleId="FootnoteReference"><w:name w:val="footnote reference"/>
  <w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>
 <w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:tblPr>
  <w:tblBorders>
   <w:top w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
   <w:left w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
   <w:bottom w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
   <w:right w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
   <w:insideH w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
   <w:insideV w:val="single" w:sz="4" w:space="0" w:color="8EAADB"/>
  </w:tblBorders>
  <w:tblCellMar><w:top w:w="60" w:type="dxa"/><w:left w:w="100" w:type="dxa"/>
   <w:bottom w:w="60" w:type="dxa"/><w:right w:w="100" w:type="dxa"/></w:tblCellMar>
 </w:tblPr></w:style>
</w:styles>"""


WORD_NUMBERING = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{W}">
 <w:abstractNum w:abstractNumId="0">
  <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/>
   <w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>
   <w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol"/></w:rPr></w:lvl>
 </w:abstractNum>
 <w:abstractNum w:abstractNumId="1">
  <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
   <w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
 </w:abstractNum>
 <w:abstractNum w:abstractNumId="2">
  <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="chineseCounting"/><w:lvlText w:val="%1、"/>
   <w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
 </w:abstractNum>
 <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
 <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
 <w:num w:numId="3"><w:abstractNumId w:val="2"/></w:num>
</w:numbering>"""


WORD_THEME = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="{A}" name="Office">
 <a:themeElements>
  <a:clrScheme name="Office">
   <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
   <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
   <a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
   <a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
   <a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4>
   <a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6>
   <a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
  </a:clrScheme>
  <a:fontScheme name="Office">
   <a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface="黑体"/><a:cs typeface=""/></a:majorFont>
   <a:minorFont><a:latin typeface="Calibri"/><a:ea typeface="宋体"/><a:cs typeface=""/></a:minorFont>
  </a:fontScheme>
  <a:fmtScheme name="Office">
   <a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst>
   <a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>
   <a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>
   <a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>
  </a:fmtScheme>
 </a:themeElements>
</a:theme>"""


def w_run(text, bold=False, italic=False, size=None, color=None, ea=None, underline=False):
    rpr = ""
    if ea:
        rpr += f'<w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="{esc(ea)}"/>'
    if bold:
        rpr += "<w:b/>"
    if italic:
        rpr += "<w:i/>"
    if underline:
        rpr += '<w:u w:val="single"/>'
    if color:
        rpr += f'<w:color w:val="{color}"/>'
    if size:
        rpr += f'<w:sz w:val="{size}"/><w:szCs w:val="{size}"/>'
    rpr = f"<w:rPr>{rpr}</w:rPr>" if rpr else ""
    return f'<w:r>{rpr}<w:t xml:space="preserve">{esc(text)}</w:t></w:r>'


def w_para(runs, style=None, num_id=None, jc=None, page_break=False, sect=None, keep_next=False):
    ppr = ""
    if style:
        ppr += f'<w:pStyle w:val="{style}"/>'
    if keep_next:
        ppr += "<w:keepNext/>"
    if page_break:
        ppr += "<w:pageBreakBefore/>"
    if num_id:
        ppr += f'<w:numPr><w:ilvl w:val="0"/><w:numId w:val="{num_id}"/></w:numPr>'
    if jc:
        ppr += f'<w:jc w:val="{jc}"/>'
    if sect:
        ppr += sect
    ppr = f"<w:pPr>{ppr}</w:pPr>" if ppr else ""
    body = "".join(runs) if isinstance(runs, (list, tuple)) else runs
    return f"<w:p>{ppr}{body}</w:p>"


def w_footnote_ref(fid):
    return (
        '<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr>'
        f'<w:footnoteReference w:id="{fid}"/></w:r>'
    )


def w_sect(columns=1, header=True, footer=True):
    refs = ""
    if header:
        refs += '<w:headerReference w:type="default" r:id="rIdHeader"/>'
    if footer:
        refs += '<w:footerReference w:type="default" r:id="rIdFooter"/>'
    return (
        f"<w:sectPr>{refs}"
        '<w:pgSz w:w="11906" w:h="16838"/>'
        '<w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" '
        'w:header="851" w:footer="992" w:gutter="0"/>'
        f'<w:cols w:num="{columns}" w:space="425"/>'
        '<w:docGrid w:type="lines" w:linePitch="312"/>'
        "</w:sectPr>"
    )


def w_graphic(rid, cx, cy, floating):
    graphic = (
        f'<a:graphic xmlns:a="{A}"><a:graphicData uri="{PIC}"><pic:pic xmlns:pic="{PIC}">'
        '<pic:nvPicPr><pic:cNvPr id="7" name="Sample"/><pic:cNvPicPr/></pic:nvPicPr>'
        f'<pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>'
        '<pic:spPr><a:xfrm><a:off x="0" y="0"/>'
        f'<a:ext cx="{cx}" cy="{cy}"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>'
        "</pic:pic></a:graphicData></a:graphic>"
    )
    if floating:
        return (
            f'<w:r><w:drawing><wp:anchor xmlns:wp="{WP}" distT="0" distB="0" '
            'distL="114300" distR="114300" simplePos="0" relativeHeight="251659264" '
            'behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">'
            '<wp:simplePos x="0" y="0"/>'
            '<wp:positionH relativeFrom="column"><wp:posOffset>3000000</wp:posOffset></wp:positionH>'
            '<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>'
            f'<wp:extent cx="{cx}" cy="{cy}"/>'
            '<wp:wrapSquare wrapText="bothSides"/>'
            '<wp:docPr id="7" name="Floating sample"/>'
            f"{graphic}</wp:anchor></w:drawing></w:r>"
        )
    return (
        f'<w:r><w:drawing><wp:inline xmlns:wp="{WP}">'
        f'<wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="8" name="Inline sample"/>'
        f"{graphic}</wp:inline></w:drawing></w:r>"
    )


def w_table(rows):
    grid = "".join(
        f'<w:gridCol w:w="{width}"/>' for width in (3200, 2900, 2600)
    )
    header = "".join(
        f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>'
        '<w:shd w:val="clear" w:color="auto" w:fill="1F4E79"/>'
        '<w:vAlign w:val="center"/></w:tcPr>'
        '<w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>'
        f'<w:r><w:rPr><w:b/><w:color w:val="FFFFFF"/></w:rPr><w:t>{esc(text)}</w:t></w:r></w:p></w:tc>'
        for text, width in zip(("项目 Item", "金额 Amount", "说明 Note"), (3200, 2900, 2600))
    )
    body_rows = []
    for i in range(1, rows + 1):
        shade = ' w:fill="F2F6FC"' if i % 2 == 0 else ""
        body_rows.append(
            f'<w:tr><w:trPr><w:cantSplit/></w:trPr>'
            f'<w:tc><w:tcPr><w:tcW w:w="3200" w:type="dxa"/></w:tcPr>'
            f'<w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>'
            f'<w:r><w:t>科目 {i} · Item {i}</w:t></w:r></w:p></w:tc>'
            f'<w:tc><w:tcPr><w:tcW w:w="2900" w:type="dxa"/>'
            f'<w:shd w:val="clear" w:color="auto"{shade}/></w:tcPr>'
            f'<w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/><w:jc w:val="right"/></w:pPr>'
            f'<w:r><w:t>{i * 1250:,}.00</w:t></w:r></w:p></w:tc>'
            f'<w:tc><w:tcPr><w:tcW w:w="2600" w:type="dxa"/></w:tcPr>'
            f'<w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>'
            f'<w:r><w:t>第 {i} 行 across pages</w:t></w:r></w:p></w:tc></w:tr>'
        )
    return (
        '<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="8700" w:type="dxa"/>'
        '<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="0" '
        'w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr>'
        f"<w:tblGrid>{grid}</w:tblGrid>"
        f'<w:tr><w:trPr><w:tblHeader/></w:trPr>{header}</w:tr>'
        + "".join(body_rows)
        + "</w:tbl>"
    )


def word_document():
    body = []
    body.append(w_para([w_run("Atrium 文档对比 / Office Diff", size=44, bold=True, ea="黑体")], style="Title", jc="center"))
    body.append(
        w_para(
            [
                w_run("本文档由 tools/office-diff/corpus/generate.py 生成，用于比较 Atrium 与 LibreOffice 的排版。", ea="宋体"),
                w_run(" It mixes Chinese and English body text so line breaks, spacing and justification can be compared."),
            ]
        )
    )
    body.append(w_para([w_run("一、概览 Overview")], style="Heading1"))
    body.append(
        w_para(
            [
                w_run("Atrium lays documents out itself and paints the text with the app's own text system, so it can redraw after each keystroke. "),
                w_run("Atrium 用自己的排版引擎计算每一行、每个字的位置，并直接绘制文字。", ea="宋体"),
            ]
        )
    )
    body.append(
        w_para(
            [w_run("床前明月光，疑是地上霜。举头望明月，低头思故乡。", italic=True, ea="宋体")],
            style="Quote",
        )
    )
    body.append(
        w_para([w_run("“Slow and steady wins the race.” — Aesop, public domain.")], style="Quote")
    )
    body.append(w_para([w_run("要点 Bullet points")], style="Heading2"))
    for text in (
        "中文与英文混排，检查字符间距与避头尾规则。",
        "Latin words of different lengths keep their inter-word spacing.",
        "行距、段前段后与首行缩进都要一致。",
        "Numbers 1234567890 and dates 2026-11-24 compare cleanly.",
    ):
        body.append(w_para([w_run(text, ea="宋体")], num_id=1))
    body.append(w_para([w_run("步骤 Steps")], style="Heading2"))
    for text in (
        "生成语料 Generate the corpus.",
        "分别渲染 Render both ways.",
        "比较并记录 Compare and record the result.",
    ):
        body.append(w_para([w_run(text, ea="宋体")], num_id=2))
    body.append(w_para([w_run("中文编号 Chinese numbering")], style="Heading2"))
    for text in ("需求调研", "方案设计", "实现与验收"):
        body.append(w_para([w_run(text, ea="宋体")], num_id=3))
    body.append(
        w_para(
            [
                w_run("页面底部有一个脚注引用", ea="宋体"),
                w_footnote_ref(1),
                w_run("，脚注文本应出现在引用所在页的底部。", ea="宋体"),
            ]
        )
    )
    body.append(w_para([w_run("二、跨页表格 Table across pages")], style="Heading1"))
    body.append(w_para([w_run("下面的表格有 38 行，表头在每一页重复。", ea="宋体")]))
    body.append(w_table(38))
    # End of section 1: one column, with the header and footer.
    body.append(w_para([], sect=w_sect(columns=1)))
    body.append(w_para([w_run("三、分栏 Columns")], style="Heading1", page_break=True))
    for i in range(6):
        body.append(
            w_para(
                [
                    w_run(
                        f"分栏正文第 {i + 1} 段。Atrium 需要按栏宽重新断行，把过长的段落流到下一栏，再流到下一页。",
                        ea="宋体",
                    ),
                    w_run(
                        " Column flow is a common source of page-count differences, so this section fills both columns deliberately."
                    ),
                ]
            )
        )
    body.append(w_para([w_run("四、图片 Pictures")], style="Heading1"))
    body.append(
        w_para(
            [
                w_run("这是一个浮动图片，文字应在它旁边绕排：", ea="宋体"),
                w_graphic("rIdImage", 1800000, 1200000, True),
                w_run(
                    " Word 会把方形环绕（square wrap）的图片放在栏内，并缩短旁边的行。Atrium 使用 Float 中的 positionH/positionV 与 wrapSquare。"
                ),
            ]
        )
    )
    body.append(
        w_para(
            [
                w_run("下面是一个嵌入图片：", ea="宋体"),
                w_graphic("rIdImage", 1440000, 960000, False),
            ]
        )
    )
    body.append(w_para([w_run("五、页眉页脚 Headers and footers")], style="Heading1"))
    body.append(
        w_para(
            [
                w_run("页眉右侧显示 “PAGE / NUMPAGES”，页脚居中显示文档名。两页的页码应连续。", ea="宋体"),
            ]
        )
    )
    body.append(
        w_para(
            [
                w_run("结尾 Closing line: Atrium office diff corpus, generated, CC0-1.0.", italic=True),
            ]
        )
    )
    body.append(w_sect(columns=2))
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:document xmlns:w="{W}" xmlns:r="{R}" xmlns:wp="{WP}" xmlns:a="{A}" xmlns:pic="{PIC}">'
        "<w:body>" + "".join(body) + "</w:body></w:document>"
    )


def word_header():
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:hdr xmlns:w="{W}" xmlns:r="{R}">'
        '<w:p><w:pPr><w:jc w:val="right"/><w:pBdr><w:bottom w:val="single" w:sz="4" w:space="1" w:color="8EAADB"/></w:pBdr></w:pPr>'
        '<w:r><w:t xml:space="preserve">Atrium office diff · Page </w:t></w:r>'
        '<w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>'
        '<w:r><w:t xml:space="preserve"> of </w:t></w:r>'
        '<w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>1</w:t></w:r></w:fldSimple>'
        "</w:p></w:hdr>"
    )


def word_footer():
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:ftr xmlns:w="{W}"><w:p><w:pPr><w:jc w:val="center"/></w:pPr>'
        '<w:r><w:t>Generated corpus · CC0-1.0</w:t></w:r></w:p></w:ftr>'
    )


def word_footnotes():
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:footnotes xmlns:w="{W}">'
        '<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>'
        '<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>'
        '<w:footnote w:id="1"><w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr>'
        '<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r>'
        '<w:r><w:t xml:space="preserve"> 脚注：这是引用所在的页底文字 (footnote text).</w:t></w:r>'
        "</w:p></w:footnote></w:footnotes>"
    )


def word_settings():
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:settings xmlns:w="{W}"><w:defaultTabStop w:val="420"/>'
        "<w:compat><w:compatSetting w:name=\"compatibilityMode\" "
        'w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat>'
        "</w:settings>"
    )


def write_docx(path):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(
            "[Content_Types].xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Types xmlns="{CT}">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Default Extension="png" ContentType="image/png"/>'
            '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
            '<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>'
            '<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>'
            '<Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>'
            '<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>'
            '<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
            '<Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>'
            '<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>'
            "</Types>",
        )
        z.writestr(
            "_rels/.rels",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Relationships xmlns="{PKG}">'
            f'<Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/>'
            "</Relationships>",
        )
        z.writestr("word/document.xml", word_document())
        z.writestr(
            "word/_rels/document.xml.rels",
            rels(
                [
                    ("rIdStyles", "styles", "styles.xml"),
                    ("rIdNumbering", "numbering", "numbering.xml"),
                    ("rIdTheme", "theme", "theme/theme1.xml"),
                    ("rIdSettings", "settings", "settings.xml"),
                    ("rIdHeader", "header", "header1.xml"),
                    ("rIdFooter", "footer", "footer1.xml"),
                    ("rIdFootnotes", "footnotes", "footnotes.xml"),
                    ("rIdImage", "image", "media/image1.png"),
                ]
            ),
        )
        z.writestr("word/styles.xml", WORD_STYLES)
        z.writestr("word/numbering.xml", WORD_NUMBERING)
        z.writestr("word/theme/theme1.xml", WORD_THEME)
        z.writestr("word/settings.xml", word_settings())
        z.writestr("word/header1.xml", word_header())
        z.writestr("word/footer1.xml", word_footer())
        z.writestr("word/footnotes.xml", word_footnotes())
        z.writestr(
            "word/_rels/header1.xml.rels",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG}"/>',
        )
        z.writestr(
            "word/_rels/footer1.xml.rels",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG}"/>',
        )
        z.writestr("word/media/image1.png", sample_png())
    return path


# ---------------------------------------------------------------- slides

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
DGM = "http://schemas.openxmlformats.org/drawingml/2006/diagram"


def sp(id_, x, y, cx, cy, fill=None, line=None, text=None, prst="rect", extra=""):
    body = ""
    if text is not None:
        body = (
            "<p:txBody><a:bodyPr/><a:lstStyle/>"
            f'<a:p><a:r><a:rPr lang="zh-CN" sz="1600"/><a:t>{esc(text)}</a:t></a:r></a:p>'
            "</p:txBody>"
        )
    return (
        "<p:sp><p:nvSpPr>"
        f'<p:cNvPr id="{id_}" name="Shape {id_}"/><p:cNvSpPr/><p:nvPr/>'
        "</p:nvSpPr><p:spPr>"
        f'<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>'
        f'<a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>'
        f"{fill or '<a:noFill/>'}{line or ''}{extra}"
        f"</p:spPr>{body}</p:sp>"
    )


def pic(id_, x, y, cx, cy, rid):
    return (
        "<p:pic><p:nvPicPr>"
        f'<p:cNvPr id="{id_}" name="Picture {id_}"/><p:cNvPicPr/><p:nvPr/>'
        "</p:nvPicPr><p:blipFill>"
        f'<a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch>'
        "</p:blipFill><p:spPr>"
        f'<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>'
        "</p:spPr></p:pic>"
    )


def connector(id_, x, y, cx, cy):
    return (
        "<p:cxnSp><p:nvCxnSpPr>"
        f'<p:cNvPr id="{id_}" name="Connector {id_}"/><p:cNvCxnSpPr/><p:nvPr/>'
        "</p:nvCxnSpPr><p:spPr>"
        f'<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>'
        '<a:prstGeom prst="straightConnector1"><a:avLst/></a:prstGeom>'
        '<a:ln w="25400"><a:solidFill><a:srgbClr val="C00000"/></a:solidFill>'
        '<a:prstDash val="dash"/><a:tailEnd type="triangle"/></a:ln>'
        "</p:spPr></p:cxnSp>"
    )


def tc(text, fill=None, span=None, vmerge=False):
    attrs = ""
    if span:
        attrs += f' gridSpan="{span}"'
    if vmerge:
        attrs += ' vMerge="1"'
    fill_xml = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else ""
    border = "".join(
        f'<a:ln{s} w="9525"><a:solidFill><a:srgbClr val="D8DEE9"/></a:solidFill></a:ln{s}>'
        for s in ("L", "R", "T", "B")
    )
    return (
        f"<a:tc{attrs}><a:txBody><a:bodyPr/><a:lstStyle/>"
        f'<a:p><a:r><a:rPr lang="zh-CN" sz="1400"/><a:t>{esc(text)}</a:t></a:r></a:p>'
        f"</a:txBody><a:tcPr marL=\"73152\" marR=\"73152\" marT=\"45720\" marB=\"45720\" anchor=\"ctr\">"
        f"<a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill>{border}{fill_xml}</a:tcPr></a:tc>"
    )


def slide_table(id_, x, y, cx, cy):
    rows = (
        f'<a:tr h="400000">{tc("项目 Item", "1F4E79", span=2)}{tc("金额", "1F4E79")}</a:tr>'
        f'<a:tr h="380000">{tc("研发 R&D")}{tc("差旅 Travel")}{tc("150,000")}</a:tr>'
        f'<a:tr h="380000">{tc("合并单元格", span=2)}{tc("25%")}</a:tr>'
    )
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Table {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">'
        '<a:tbl><a:tblPr firstRow="1" bandRow="1"/>'
        '<a:tblGrid><a:gridCol w="2000000"/><a:gridCol w="2000000"/>'
        '<a:gridCol w="1800000"/></a:tblGrid>'
        f"{rows}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
    )


def chart_frame(id_, x, y, cx, cy, rid):
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Chart {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        f'<a:graphic><a:graphicData uri="{C}"><c:chart xmlns:c="{C}" r:id="{rid}"/>'
        "</a:graphicData></a:graphic></p:graphicFrame>"
    )


def ser(name, cats, vals, color, idx=0):
    cpts = "".join(f'<c:pt idx="{i}"><c:v>{esc(v)}</c:v></c:pt>' for i, v in enumerate(cats))
    vpts = "".join(f'<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>' for i, v in enumerate(vals))
    return (
        f'<c:ser><c:idx val="{idx}"/><c:order val="{idx}"/>'
        f'<c:spPr><a:solidFill><a:srgbClr val="{color}"/></a:solidFill></c:spPr>'
        f'<c:tx><c:strRef><c:f>Sheet1!$A$1</c:f><c:strCache><c:ptCount val="1"/>'
        f'<c:pt idx="0"><c:v>{esc(name)}</c:v></c:pt></c:strCache></c:strRef></c:tx>'
        f'<c:cat><c:strRef><c:f>Sheet1!$A$2:$A${len(cats) + 1}</c:f><c:strCache>'
        f'<c:ptCount val="{len(cats)}"/>{cpts}</c:strCache></c:strRef></c:cat>'
        f'<c:val><c:numRef><c:f>Sheet1!$B$2:$B${len(vals) + 1}</c:f><c:numCache>'
        f'<c:formatCode>General</c:formatCode>'
        f'<c:ptCount val="{len(vals)}"/>{vpts}</c:numCache></c:numRef></c:val></c:ser>'
    )


def chart_xml(kind, series, title, bar_dir=None, grouping=None, labels=False):
    head = f"<c:{kind}>"
    if bar_dir:
        head += f'<c:barDir val="{bar_dir}"/>'
    if grouping:
        head += f'<c:grouping val="{grouping}"/>'
    if labels:
        head += '<c:dLbls><c:showVal val="1"/><c:showCatName val="0"/></c:dLbls>'
    # Pie and doughnut charts take no axes; bar/line/area charts need both.
    axes = (
        ""
        if kind in ("pieChart", "doughnutChart", "pie3DChart")
        else (
            '<c:catAx><c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling>'
            '<c:delete val="0"/><c:axPos val="b"/><c:crossAx val="2"/></c:catAx>'
            '<c:valAx><c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling>'
            '<c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/><c:crossAx val="1"/>'
            "</c:valAx>"
        )
    )
    return (
        f'<c:chartSpace xmlns:c="{C}" xmlns:a="{A}" xmlns:r="{R}"><c:chart>'
        f'<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{esc(title)}</a:t></a:r></a:p>'
        '</c:rich></c:tx><c:overlay val="0"/></c:title>'
        '<c:autoTitleDeleted val="0"/><c:plotArea><c:layout/>'
        + head
        + "".join(series)
        + "</c:"
        + kind
        + ">"
        + axes
        + "</c:plotArea>"
        '<c:legend><c:legendPos val="r"/><c:overlay val="0"/></c:legend>'
        "</c:chart></c:chartSpace>"
    )


def slide_one():
    rail = '<a:solidFill><a:srgbClr val="1F4E79"/></a:solidFill>'
    orange = '<a:solidFill><a:srgbClr val="ED7D31"/></a:solidFill>'
    green = '<a:solidFill><a:srgbClr val="70AD47"/></a:solidFill>'
    grad = (
        "<a:gradFill><a:gsLst><a:gs pos=\"0\"><a:srgbClr val=\"4472C4\"/></a:gs>"
        '<a:gs pos="100000"><a:srgbClr val="5B9BD5"/></a:gs></a:gsLst>'
        '<a:lin ang="5400000"/></a:gradFill>'
    )
    group = (
        '<p:grpSp><p:nvGrpSpPr><p:cNvPr id="20" name="Group"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
        '<p:grpSpPr><a:xfrm><a:off x="6000000" y="1150000"/><a:ext cx="2800000" cy="1000000"/>'
        '<a:chOff x="0" y="0"/><a:chExt cx="2000000" cy="1000000"/></a:xfrm></p:grpSpPr>'
        + sp(21, 0, 0, 1000000, 900000, fill=grad, text="Grouped 组合")
        + sp(22, 1000000, 250000, 1000000, 700000, fill=green, prst="roundRect", text="Round")
        + "</p:grpSp>"
    )
    bullets = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"12\" name=\"Bullets\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>"
        '<p:spPr><a:xfrm><a:off x="2100000" y="2350000"/><a:ext cx="3800000" cy="1200000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>'
        '<p:txBody><a:bodyPr wrap="square"/><a:lstStyle/>'
        '<a:p><a:pPr marL="342900" indent="-342900"><a:buChar char="•"/></a:pPr>'
        '<a:r><a:rPr lang="zh-CN" sz="1800"/><a:t>中文项目符号 Bullet one</a:t></a:r></a:p>'
        '<a:p><a:pPr marL="342900" indent="-342900"><a:buChar char="•"/></a:pPr>'
        '<a:r><a:rPr lang="en-US" sz="1800"/><a:t>Latin bullet with several words</a:t></a:r></a:p>'
        "</p:txBody></p:sp>"
    )
    return (
        f'<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        + sp(10, 400000, 300000, 8200000, 750000, fill=rail, text="季度对比 Quarterly review")
        + sp(11, 400000, 1200000, 1600000, 700000, fill=orange, prst="ellipse", text="Ellipse")
        + sp(13, 2200000, 1200000, 1500000, 700000, fill=green, prst="rightArrow")
        + sp(14, 3900000, 1200000, 1200000, 700000, fill=orange, prst="chevron")
        + sp(15, 5300000, 1200000, 900000, 700000, fill=grad, prst="cloud")
        + sp(16, 6500000, 1200000, 800000, 800000, fill=orange, prst="star5")
        + group
        + connector(23, 400000, 2100000, 1200000, 200000)
        + bullets
        + pic(30, 400000, 2350000, 1500000, 1000000, "rIdImg")
        + slide_table(40, 400000, 3700000, 5800000, 1100000)
        + chart_frame(50, 6300000, 2350000, 2500000, 2300000, "rIdChart1")
        + "</p:spTree></p:cSld>"
        '<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>'
    )


def slide_two():
    return (
        f'<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld>'
        '<p:bg><p:bgPr><a:gradFill><a:gsLst>'
        '<a:gs pos="0"><a:srgbClr val="EEF3FF"/></a:gs>'
        '<a:gs pos="100000"><a:srgbClr val="FFF4E8"/></a:gs></a:gsLst>'
        '<a:lin ang="2700000"/></a:gradFill></p:bgPr></p:bg>'
        '<p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        + sp(10, 400000, 250000, 6000000, 600000, text="图表 Charts")
        + chart_frame(51, 400000, 900000, 2600000, 2100000, "rIdChart2")
        + chart_frame(52, 3100000, 900000, 2600000, 2100000, "rIdChart3")
        + chart_frame(53, 5800000, 900000, 2600000, 2100000, "rIdChart4")
        + sp(54, 400000, 3300000, 7900000, 500000, fill='<a:solidFill><a:srgbClr val="DDEBF7"/></a:solidFill>',
             text="图例与数据标签 Legend and data labels")
        + "</p:spTree></p:cSld>"
        '<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>'
    )


def slide_theme():
    return WORD_THEME.replace("wordprocessingml", "drawingml") if False else f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="{A}" name="Office">
 <a:themeElements>
  <a:clrScheme name="Office">
   <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
   <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
   <a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
   <a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
   <a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4>
   <a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6>
   <a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
  </a:clrScheme>
  <a:fontScheme name="Office">
   <a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface="黑体"/><a:cs typeface=""/></a:majorFont>
   <a:minorFont><a:latin typeface="Calibri"/><a:ea typeface="宋体"/><a:cs typeface=""/></a:minorFont>
  </a:fontScheme>
  <a:fmtScheme name="Office">
   <a:fillStyleLst>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
   </a:fillStyleLst>
   <a:lnStyleLst>
    <a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>
    <a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>
    <a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>
   </a:lnStyleLst>
   <a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle>
    <a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>
   <a:bgFillStyleLst>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
   </a:bgFillStyleLst>
  </a:fmtScheme>
 </a:themeElements>
</a:theme>"""


def slide_master():
    return (
        f'<p:sldMaster xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld>'
        '<p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg>'
        '<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>'
        '<p:spPr><a:xfrm><a:off x="400000" y="250000"/><a:ext cx="8200000" cy="700000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Master title</a:t></a:r></a:p></p:txBody></p:sp>'
        + pic(3, 7800000, 150000, 900000, 600000, "rIdLogo")
        + "</p:spTree></p:cSld>"
        '<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/>'
        "</p:sldLayoutIdLst>"
        '<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" '
        'accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" '
        'hlink="hlink" folHlink="folHlink"/>'
        '<p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="3200" b="1">'
        '<a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="Calibri Light"/>'
        '<a:ea typeface="黑体"/></a:defRPr></a:lvl1pPr></p:titleStyle>'
        '<p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:defRPr sz="1800">'
        '<a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="Calibri"/>'
        '<a:ea typeface="宋体"/></a:defRPr><a:buChar char="•"/></a:lvl1pPr></p:bodyStyle>'
        '<p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle></p:txStyles>'
        "</p:sldMaster>"
    )


def slide_layout():
    return (
        f'<p:sldLayout xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}" type="titleOnly">'
        '<p:cSld name="Title Only"><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>'
        '<p:spPr><a:xfrm><a:off x="400000" y="250000"/><a:ext cx="8200000" cy="700000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Layout title</a:t></a:r></a:p></p:txBody></p:sp>'
        "</p:spTree></p:cSld></p:sldLayout>"
    )


def write_pptx(path):
    charts = {
        "ppt/charts/chart1.xml": chart_xml(
            "barChart",
            [
                ser("收入 Revenue", ["Q1", "Q2", "Q3", "Q4"], [12, 18, 15, 24], "4472C4", 0),
                ser("成本 Cost", ["Q1", "Q2", "Q3", "Q4"], [8, 11, 9, 14], "ED7D31", 1),
            ],
            "季度收入与成本",
            bar_dir="col",
            grouping="clustered",
            labels=True,
        ),
        "ppt/charts/chart2.xml": chart_xml(
            "pieChart",
            [ser("Share", ["研发", "市场", "运营"], [45, 30, 25], "4472C4")],
            "支出构成",
            labels=True,
        ),
        "ppt/charts/chart3.xml": chart_xml(
            "lineChart",
            [ser("Line", ["A", "B", "C", "D", "E"], [2, 5, 3, 8, 6], "5B9BD5")],
            "趋势 Trend",
            labels=False,
        ),
        "ppt/charts/chart4.xml": chart_xml(
            "barChart",
            [ser("Bar", ["East", "West", "North"], [5, 3, 4], "8064A2")],
            "区域 Region",
            bar_dir="bar",
            grouping="clustered",
            labels=True,
        ),
    }
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(
            "[Content_Types].xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Types xmlns="{CT}">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Default Extension="png" ContentType="image/png"/>'
            '<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>'
            '<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
            '<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
            '<Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/>'
            '<Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/>'
            '<Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>'
            '<Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/>'
            + "".join(
                f'<Override PartName="/{name}" '
                'ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>'
                for name in charts
            )
            + "</Types>",
        )
        z.writestr(
            "_rels/.rels",
            rels([("rId1", "officeDocument", "ppt/presentation.xml")]),
        )
        z.writestr(
            "ppt/presentation.xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<p:presentation xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}">'
            '<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
            '<p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst>'
            '<p:sldSz cx="9144000" cy="5143500"/><p:notesSz cx="6858000" cy="9144000"/>'
            "</p:presentation>",
        )
        z.writestr(
            "ppt/_rels/presentation.xml.rels",
            rels(
                [
                    ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
                    ("rId2", "slide", "slides/slide1.xml"),
                    ("rId3", "slide", "slides/slide2.xml"),
                    ("rId4", "theme", "theme/theme1.xml"),
                    ("rId5", "tableStyles", "tableStyles.xml"),
                ]
            ),
        )
        z.writestr("ppt/theme/theme1.xml", slide_theme())
        z.writestr(
            "ppt/tableStyles.xml",
            f'<a:tblStyleLst xmlns:a="{A}" def="{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}"/>',
        )
        z.writestr("ppt/slideMasters/slideMaster1.xml", slide_master())
        z.writestr(
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            rels(
                [
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rId2", "theme", "../theme/theme1.xml"),
                    ("rIdLogo", "image", "../media/logo.png"),
                ]
            ),
        )
        z.writestr("ppt/slideLayouts/slideLayout1.xml", slide_layout())
        z.writestr(
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            rels([("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]),
        )
        z.writestr("ppt/slides/slide1.xml", slide_one())
        z.writestr(
            "ppt/slides/_rels/slide1.xml.rels",
            rels(
                [
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rIdImg", "image", "../media/logo.png"),
                    ("rIdChart1", "chart", "../charts/chart1.xml"),
                ]
            ),
        )
        z.writestr("ppt/slides/slide2.xml", slide_two())
        z.writestr(
            "ppt/slides/_rels/slide2.xml.rels",
            rels(
                [
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rIdChart2", "chart", "../charts/chart2.xml"),
                    ("rIdChart3", "chart", "../charts/chart3.xml"),
                    ("rIdChart4", "chart", "../charts/chart4.xml"),
                ]
            ),
        )
        for name, xml in charts.items():
            z.writestr(name, xml)
        z.writestr("ppt/media/logo.png", sample_png())
    return path


# ---------------------------------------------------------------- sheets

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
RS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
XDR = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"


def sheet_styles():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS}">
 <numFmts count="8">
  <numFmt numFmtId="164" formatCode="yyyy&quot;年&quot;m&quot;月&quot;d&quot;日&quot;"/>
  <numFmt numFmtId="165" formatCode="[Red]0.00"/>
  <numFmt numFmtId="166" formatCode="0.00;[Red]-0.00"/>
  <numFmt numFmtId="167" formatCode="&quot;¥&quot;#,##0.00"/>
  <numFmt numFmtId="168" formatCode="# ?/?"/>
  <numFmt numFmtId="169" formatCode="0.00E+00"/>
  <numFmt numFmtId="170" formatCode="[$-804]#,##0.00"/>
  <numFmt numFmtId="171" formatCode="上午/下午 h:mm"/>
 </numFmts>
 <fonts count="6">
  <font><sz val="11"/><color theme="1"/><name val="Calibri"/><family val="2"/><scheme val="minor"/></font>
  <font><b/><sz val="12"/><color rgb="FFFFFFFF"/><name val="Calibri"/></font>
  <font><b/><sz val="12"/><color rgb="FFC00000"/><name val="宋体"/><charset val="134"/></font>
  <font><i/><sz val="11"/><color rgb="FF0000FF"/><name val="等线"/><charset val="134"/></font>
  <font><b/><sz val="14"/><color rgb="FF1F4E79"/><name val="Calibri Light"/></font>
  <font><sz val="10"/><color rgb="FF7F7F7F"/><name val="Calibri"/></font>
 </fonts>
 <fills count="4">
  <fill><patternFill patternType="none"/></fill>
  <fill><patternFill patternType="gray125"/></fill>
  <fill><patternFill patternType="solid"><fgColor rgb="FF1F4E79"/><bgColor indexed="64"/></patternFill></fill>
  <fill><patternFill patternType="solid"><fgColor rgb="FFFFF2CC"/><bgColor indexed="64"/></patternFill></fill>
 </fills>
 <borders count="3">
  <border><left/><right/><top/><bottom/><diagonal/></border>
  <border><left style="thin"><color rgb="FF8EAADB"/></left><right style="thin"><color rgb="FF8EAADB"/></right>
   <top style="thin"><color rgb="FF8EAADB"/></top><bottom style="thin"><color rgb="FF8EAADB"/></bottom><diagonal/></border>
  <border><left/><right/><top/><bottom style="double"><color rgb="FF000000"/></bottom><diagonal/></border>
 </borders>
 <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
 <cellXfs count="20">
  <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
  <xf numFmtId="0" fontId="1" fillId="2" borderId="1" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf>
  <xf numFmtId="10" fontId="3" fillId="0" borderId="0" xfId="0" applyNumberFormat="1" applyFont="1"><alignment horizontal="right"/></xf>
  <xf numFmtId="31" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="165" fontId="2" fillId="0" borderId="0" xfId="0" applyNumberFormat="1" applyFont="1"/>
  <xf numFmtId="166" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="167" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="168" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="169" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="170" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="171" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="4" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="11" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="0" fontId="4" fillId="3" borderId="2" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf>
  <xf numFmtId="2" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1" applyAlignment="1"><alignment horizontal="right"/></xf>
  <xf numFmtId="9" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="18" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="0" fontId="5" fillId="0" borderId="0" xfId="0" applyFont="1"/>
  <xf numFmtId="0" fontId="3" fillId="0" borderId="0" xfId="0" applyFont="1" applyAlignment="1"><alignment vertical="top" wrapText="1"/></xf>
 </cellXfs>
 <cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
 <dxfs count="4">
  <dxf><fill><patternFill><fgColor rgb="FFC6EFCE"/></patternFill></fill></dxf>
  <dxf><font><b/><color rgb="FF9C0006"/></font></dxf>
  <dxf><fill><patternFill><fgColor rgb="FFFFEB9C"/></patternFill></fill></dxf>
  <dxf><font><color rgb="FF0000FF"/></font></dxf>
 </dxfs>
 <tableStyles count="0" defaultTableStyle="TableStyleMedium2" defaultPivotStyle="PivotStyleLight16"/>
</styleSheet>"""


SHARED = [
    "预算 Budget",
    "说明 Notes",
    "季度 Quarter",
    "研发 R&D",
    "差旅 Travel",
    "合计 Total",
]


def shared_strings(items):
    body = "".join(f'<si><t xml:space="preserve">{esc(s)}</t></si>' for s in items)
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<sst xmlns="{NS}" count="{len(items)}" uniqueCount="{len(items)}">{body}</sst>'
    )


def sheet_xml():
    rows = [
        # Header row, merged A1:C1.
        '<row r="1" ht="26" customHeight="1">'
        '<c r="A1" s="1" t="s"><v>0</v></c><c r="B1" s="1"/><c r="C1" s="1"/>'
        '<c r="D1" s="14" t="s"><v>2</v></c></row>',
        # Number formats, one family per cell.
        '<row r="2" ht="20" customHeight="1">'
        '<c r="A2" s="2"><v>0.25</v></c>'
        '<c r="B2" s="7"><v>1234.5</v></c>'
        '<c r="C2" s="9"><v>0.000125</v></c>'
        '<c r="D2" s="8"><v>1234.5</v></c></row>',
        # Hidden row: red, date, date+time, formula.
        '<row r="3" hidden="1">'
        '<c r="A3" s="5"><v>1.25</v></c>'
        '<c r="B3" s="4"><v>45356</v></c>'
        '<c r="C3" s="16"><v>45356.625</v></c>'
        '<c r="D3" s="18"><f>SUM(B2:B3)</f><v>1234.5</v></c></row>',
        '<row r="4">'
        '<c r="A4" s="10"><v>45356</v></c>'
        '<c r="B4" s="6"><v>-12.5</v></c>'
        '<c r="C4" s="7"><v>98765.43</v></c>'
        '<c r="D4" s="11"><v>0.8125</v></c></row>',
        # A tall styled row.
        '<row r="5" ht="30" customHeight="1">'
        '<c r="A5" s="15"><v>100</v></c><c r="B5" s="12"><v>1234.5</v></c>'
        '<c r="C5" s="13"><v>0.0000005</v></c><c r="D5" s="3"><v>1234567.89</v></c></row>',
        '<row r="6"><c r="A6" s="17" t="s"><v>1</v></c><c r="B6"><v>10</v></c>'
        '<c r="C6" s="19" t="s"><v>3</v></c></row>',
        # Data for the conditional formats.
        '<row r="7"><c r="A7"><v>1</v></c><c r="B7"><v>2</v></c><c r="C7"><v>3</v></c><c r="D7"><v>5</v></c></row>',
        '<row r="8"><c r="A8"><v>5</v></c><c r="B8"><v>10</v></c><c r="C8"><v>9</v></c><c r="D8"><v>10</v></c>'
        '<c r="E8" t="s"><v>1</v></c></row>',
        '<row r="9" hidden="1"><c r="A9"><v>5</v></c><c r="C9"><v>8</v></c></row>',
    ]
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<worksheet xmlns="{NS}" xmlns:r="{RS}">'
        '<sheetPr><tabColor rgb="FF4472C4"/></sheetPr>'
        '<dimension ref="A1:F14"/>'
        '<sheetViews><sheetView showGridLines="0" zoomScale="120" workbookViewId="0">'
        '<pane xSplit="1" ySplit="1" topLeftCell="B2" activePane="bottomRight" state="frozen"/>'
        "</sheetView></sheetViews>"
        '<sheetFormatPr defaultColWidth="8.43" defaultRowHeight="15"/>'
        "<cols>"
        '<col min="1" max="1" width="22" customWidth="1"/>'
        '<col min="2" max="2" width="16" customWidth="1"/>'
        '<col min="3" max="3" width="8.43" hidden="1" customWidth="1"/>'
        '<col min="4" max="4" width="20" customWidth="1"/>'
        '<col min="5" max="5" width="14" customWidth="1"/>'
        "</cols>"
        f'<sheetData>{"".join(rows)}</sheetData>'
        '<mergeCells count="1"><mergeCell ref="A1:C1"/></mergeCells>'
        '<conditionalFormatting sqref="D8"><cfRule type="cellIs" operator="greaterThan" priority="1" dxfId="0">'
        "<formula>0</formula></cfRule></conditionalFormatting>"
        '<conditionalFormatting sqref="A6"><cfRule type="containsText" text="明" priority="2" dxfId="1" '
        'operator="containsText"><formula>NOT(ISERROR(SEARCH("明",A6)))</formula></cfRule></conditionalFormatting>'
        '<conditionalFormatting sqref="B6:B7"><cfRule type="top10" rank="1" priority="3" dxfId="2"/></conditionalFormatting>'
        '<conditionalFormatting sqref="A7:C7"><cfRule type="colorScale" priority="4"><colorScale>'
        '<cfvo type="min"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FF63BE7B"/>'
        "</colorScale></cfRule></conditionalFormatting>"
        '<conditionalFormatting sqref="D7:D8"><cfRule type="dataBar" priority="5"><dataBar>'
        '<cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar></cfRule>'
        "</conditionalFormatting>"
        '<drawing r:id="rIdDraw"/>'
        "</worksheet>"
    )


def sheet_drawing():
    return (
        f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<xdr:wsDr xmlns:xdr="{XDR}" xmlns:a="{A}" xmlns:r="{RS}" xmlns:c="{C}">'
        "<xdr:twoCellAnchor>"
        "<xdr:from><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>9</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>"
        "<xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>12</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>"
        "<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"图片 1\"/><xdr:cNvPicPr/></xdr:nvPicPr>"
        '<xdr:blipFill><a:blip r:embed="rIdPic"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill>'
        '<xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="1500000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:twoCellAnchor>'
        "<xdr:oneCellAnchor>"
        "<xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>13</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>"
        '<xdr:ext cx="4200000" cy="2500000"/>'
        '<xdr:graphicFrame macro=""><xdr:nvGraphicFramePr>'
        '<xdr:cNvPr id="3" name="图表 1"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>'
        '<xdr:xfrm><a:off x="0" y="0"/><a:ext cx="4200000" cy="2500000"/></xdr:xfrm>'
        f'<a:graphic><a:graphicData uri="{C}"><c:chart r:id="rIdChart"/></a:graphicData></a:graphic>'
        "</xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor>"
        "<xdr:twoCellAnchor>"
        "<xdr:from><xdr:col>6</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>"
        "<xdr:to><xdr:col>8</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>4</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>"
        '<xdr:sp macro="" textlink=""><xdr:nvSpPr><xdr:cNvPr id="4" name="文本框 1"/>'
        '<xdr:cNvSpPr txBox="1"/></xdr:nvSpPr>'
        '<xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2200000" cy="1200000"/></a:xfrm>'
        '<a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom>'
        '<a:solidFill><a:srgbClr val="FFF2F2F2"/></a:solidFill>'
        '<a:ln w="12700"><a:solidFill><a:srgbClr val="FF4472C4"/></a:solidFill></a:ln></xdr:spPr>'
        '<xdr:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN"/><a:t>备注说明</a:t></a:r>'
        "</a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:twoCellAnchor>"
        "</xdr:wsDr>"
    )


def sheet_chart():
    return chart_xml(
        "barChart",
        [
            ser("收入 Revenue", ["Q1", "Q2", "Q3"], [10, 20, 30], "4472C4", 0),
            ser("成本 Cost", ["Q1", "Q2", "Q3"], [6, 12, 18], "ED7D31", 1),
        ],
        "季度 Quarter",
        bar_dir="col",
        grouping="clustered",
        labels=True,
    )


def write_xlsx(path):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(
            "[Content_Types].xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<Types xmlns="{CT}">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Default Extension="png" ContentType="image/png"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
            '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
            '<Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>'
            '<Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>'
            '<Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>'
            "</Types>",
        )
        z.writestr(
            "_rels/.rels",
            rels([("rId1", "officeDocument", "xl/workbook.xml")]),
        )
        z.writestr(
            "xl/workbook.xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<workbook xmlns="{NS}" xmlns:r="{RS}"><workbookPr/>'
            '<sheets><sheet name="预算" sheetId="1" r:id="rId1"/></sheets>'
            '<calcPr calcId="191029"/></workbook>',
        )
        z.writestr(
            "xl/_rels/workbook.xml.rels",
            rels(
                [
                    ("rId1", "worksheet", "worksheets/sheet1.xml"),
                    ("rId2", "styles", "styles.xml"),
                    ("rId3", "sharedStrings", "sharedStrings.xml"),
                    ("rId4", "theme", "theme/theme1.xml"),
                ]
            ),
        )
        z.writestr("xl/theme/theme1.xml", WORD_THEME)
        z.writestr("xl/styles.xml", sheet_styles())
        z.writestr("xl/sharedStrings.xml", shared_strings(SHARED))
        z.writestr("xl/worksheets/sheet1.xml", sheet_xml())
        z.writestr(
            "xl/worksheets/_rels/sheet1.xml.rels",
            rels([("rIdDraw", "drawing", "../drawings/drawing1.xml")]),
        )
        z.writestr("xl/drawings/drawing1.xml", sheet_drawing())
        z.writestr(
            "xl/drawings/_rels/drawing1.xml.rels",
            rels(
                [
                    ("rIdPic", "image", "../media/image1.png"),
                    ("rIdChart", "chart", "../charts/chart1.xml"),
                ]
            ),
        )
        z.writestr("xl/charts/chart1.xml", sheet_chart())
        z.writestr("xl/media/image1.png", sample_png())
    return path


# ---------------------------------------------------------------- main


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"),
        help="directory to write the corpus into",
    )
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)
    written = [
        write_docx(os.path.join(args.out, "word-bilingual.docx")),
        write_pptx(os.path.join(args.out, "slides-shapes.pptx")),
        write_xlsx(os.path.join(args.out, "sheet-formats.xlsx")),
    ]
    for path in written:
        print(path)


if __name__ == "__main__":
    main()
