#!/usr/bin/env python3
"""Build the slides-full fixture deck as raw OOXML.

Provenance: this deck was generated programmatically here (no PowerPoint or
WPS was available in the container). It is deliberately small and readable,
and covers groups, crop, gradient, a styled merged table, charts and a
master logo.
"""

import base64
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "deck.pptx")

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
DGM = "http://schemas.openxmlformats.org/drawingml/2006/diagram"
REL = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl2Z7sAAAAASUVORK5CYII="
)

def rect(id_, x, y, cx, cy, fill=None, line=None, text=None, prst="rect", extra_sp=""):
    fill_xml = fill or '<a:noFill/>'
    line_xml = line or ""
    body = ""
    if text is not None:
        body = (
            "<p:txBody><a:bodyPr/><a:lstStyle/>"
            f'<a:p><a:r><a:rPr lang="en-US"/><a:t>{text}</a:t></a:r></a:p></p:txBody>'
        )
    return (
        "<p:sp><p:nvSpPr>"
        f'<p:cNvPr id="{id_}" name="Shape {id_}"/><p:cNvSpPr/><p:nvPr/>'
        "</p:nvSpPr><p:spPr>"
        f'<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>'
        f'<a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>{fill_xml}{line_xml}{extra_sp}'
        f"</p:spPr>{body}</p:sp>"
    )

def pic(id_, x, y, cx, cy, rid, crop=None):
    src = ""
    if crop:
        src = f'<a:srcRect t="{crop[0]}" r="{crop[1]}" b="{crop[2]}" l="{crop[3]}"/>'
    return (
        "<p:pic><p:nvPicPr>"
        f'<p:cNvPr id="{id_}" name="Picture {id_}"/><p:cNvPicPr/><p:nvPr/>'
        "</p:nvPicPr><p:blipFill>"
        f'<a:blip r:embed="{rid}"/>{src}<a:stretch><a:fillRect/></a:stretch>'
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
        '<a:ln w="12700"><a:solidFill><a:srgbClr val="C00000"/></a:solidFill>'
        '<a:prstDash val="dot"/><a:tailEnd type="triangle"/></a:ln>'
        "</p:spPr></p:cxnSp>"
    )

def tc(text, fill=None, borders=True, span=None, vmerge=False, rowspan=None):
    attrs = ""
    if span:
        attrs += f' gridSpan="{span}"'
    if vmerge:
        attrs += ' vMerge="1"'
    if rowspan:
        attrs += f' rowSpan="{rowspan}"'
    fill_xml = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else ""
    border = '<a:lnL w="9525"><a:solidFill><a:srgbClr val="D8DEE9"/></a:solidFill></a:lnL>' if borders else ""
    border += '<a:lnR w="9525"><a:solidFill><a:srgbClr val="D8DEE9"/></a:solidFill></a:lnR>' if borders else ""
    border += '<a:lnT w="9525"><a:solidFill><a:srgbClr val="D8DEE9"/></a:solidFill></a:lnT>' if borders else ""
    border += '<a:lnB w="9525"><a:solidFill><a:srgbClr val="D8DEE9"/></a:solidFill></a:lnB>' if borders else ""
    return (
        f"<a:tc{attrs}><a:txBody><a:bodyPr/><a:lstStyle/>"
        f'<a:p><a:r><a:rPr lang="en-US"/><a:t>{text}</a:t></a:r></a:p>'
        f'</a:txBody><a:tcPr marL="73152" marR="73152" marT="45720" marB="45720" anchor="ctr">{border}{fill_xml}</a:tcPr></a:tc>'
    )

def table(id_, x, y, cx, cy):
    rows = (
        f'<a:tr h="400000">{tc("Merged", "1E2761", True, span=2)}{tc("C", "1E2761")}</a:tr>'
        f'<a:tr h="400000">{tc("A", rowspan=2)}{tc("B")}{tc("C")}</a:tr>'
        f'<a:tr h="400000">{tc("", vmerge=True)}{tc("D")}{tc("E")}</a:tr>'
    )
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Table {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">'
        '<a:tbl><a:tblPr firstRow="1" bandRow="1">'
        "<a:tableStyleId>{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}</a:tableStyleId>"
        '</a:tblPr><a:tblGrid><a:gridCol w="2000000"/><a:gridCol w="2000000"/><a:gridCol w="2000000"/></a:tblGrid>'
        f"{rows}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
    )

def chart_frame(id_, x, y, cx, cy, rid):
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Chart {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        f'<a:graphic><a:graphicData uri="{C}">'
        f'<c:chart xmlns:c="{C}" r:id="{rid}"/>'
        "</a:graphicData></a:graphic></p:graphicFrame>"
    )

def diagram_frame(id_, x, y, cx, cy):
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Diagram {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram">'
        f'<dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="rIdDgm" r:lo="rIdDgm" r:qs="rIdDgm" r:cs="rIdDgm"/>'
        "</a:graphicData></a:graphic></p:graphicFrame>"
    )

def ole_frame(id_, x, y, cx, cy, rid):
    return (
        "<p:graphicFrame><p:nvGraphicFramePr>"
        f'<p:cNvPr id="{id_}" name="Object {id_}"/><p:cNvGraphicFramePr/><p:nvPr/>'
        "</p:nvGraphicFramePr>"
        f'<p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></p:xfrm>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/presentationml/2006/ole">'
        '<p:oleObj spid="_x0000_s1026" name="Object" imgW="1000000" imgH="500000">'
        '<p:embed/>'
        f'<p:pic><p:nvPicPr><p:cNvPr id="{id_ + 1}" name="Fallback"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr>'
        f'<p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>'
        '<p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1000000" cy="500000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>'
        "</p:oleObj></a:graphicData></a:graphic></p:graphicFrame>"
    )

def ser(name, cats, vals, color=None):
    color_xml = (
        f'<c:spPr><a:solidFill><a:srgbClr val="{color}"/></a:solidFill></c:spPr>'
        if color
        else ""
    )
    cpts = "".join(f'<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>' for i, v in enumerate(cats))
    vpts = "".join(f'<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>' for i, v in enumerate(vals))
    return (
        f"<c:ser><c:idx val=\"0\"/><c:order val=\"0\"/>{color_xml}"
        f'<c:tx><c:strRef><c:f>Sheet1!$A$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>{name}</c:v></c:pt></c:strCache></c:strRef></c:tx>'
        f'<c:cat><c:strRef><c:f>Sheet1!$A$2:$A$4</c:f><c:strCache><c:ptCount val="{len(cats)}"/>{cpts}</c:strCache></c:strRef></c:cat>'
        f'<c:val><c:numRef><c:f>Sheet1!$B$2:$B$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="{len(vals)}"/>{vpts}</c:numCache></c:numRef></c:val>'
        "</c:ser>"
    )

def chart(kind, cats, series, title="Chart", grouping=None, labels=False, bar_dir=None):
    type_xml = f"<c:{kind}>"
    if bar_dir:
        type_xml += f'<c:barDir val="{bar_dir}"/>'
    if grouping:
        type_xml += f'<c:grouping val="{grouping}"/>'
    if labels:
        type_xml += '<c:dLbls><c:showVal val="1"/></c:dLbls>'
    type_xml += "".join(series) + "</c:" + kind + ">"
    return (
        f'<c:chartSpace xmlns:c="{C}" xmlns:a="{A}" xmlns:r="{R}"><c:chart>'
        f'<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{title}</a:t></a:r></a:p></c:rich></c:tx></c:title>'
        f"<c:plotArea><c:layout/>{type_xml}"
        '<c:catAx><c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/></c:catAx>'
        '<c:valAx><c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/></c:valAx>'
        "</c:plotArea>"
        '<c:legend><c:legendPos val="r"/></c:legend>'
        "</c:chart></c:chartSpace>"
    )

def slide1():
    grad = (
        "<a:gradFill><a:gsLst>"
        '<a:gs pos="0"><a:srgbClr val="4472C4"/></a:gs>'
        '<a:gs pos="100000"><a:srgbClr val="ED7D31"/></a:gs>'
        '</a:gsLst><a:lin ang="5400000" scaled="1"/></a:gradFill>'
    )
    line = (
        '<a:ln w="25400"><a:solidFill><a:srgbClr val="C00000"/></a:solidFill>'
        '<a:prstDash val="dash"/><a:headEnd type="triangle"/><a:tailEnd type="oval"/></a:ln>'
    )
    shadow = (
        '<a:effectLst><a:outerShdw blurRad="40000" dist="30000" dir="2700000">'
        '<a:srgbClr val="000000"><a:alpha val="50000"/></a:srgbClr></a:outerShdw></a:effectLst>'
    )
    group = (
        "<p:grpSp><p:nvGrpSpPr><p:cNvPr id=\"20\" name=\"Group\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>"
        '<p:grpSpPr><a:xfrm><a:off x="5600000" y="500000"/><a:ext cx="3000000" cy="1200000"/>'
        '<a:chOff x="0" y="0"/><a:chExt cx="2000000" cy="800000"/></a:xfrm></p:grpSpPr>'
        + rect(21, 0, 0, 1000000, 800000, fill=grad, line=line, extra_sp=shadow, text="Grouped")
        + rect(22, 1000000, 200000, 1000000, 600000, fill='<a:solidFill><a:srgbClr val="70AD47"/></a:solidFill>', prst="roundRect", text="Round")
        + "</p:grpSp>"
    )
    style_shape = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"30\" name=\"Styled\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>"
        "<p:spPr><a:xfrm><a:off x=\"5600000\" y=\"1900000\"/><a:ext cx=\"1400000\" cy=\"600000\"/></a:xfrm>"
        '<a:prstGeom prst="hexagon"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:style><a:lnRef idx="2"><a:schemeClr val="accent2"/></a:lnRef>'
        '<a:fillRef idx="1"><a:schemeClr val="accent1"/></a:fillRef>'
        '<a:effectRef idx="0"><a:schemeClr val="accent1"/></a:effectRef>'
        '<a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef></p:style>'
        "<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\" sz=\"1400\"/><a:t>Style</a:t></a:r></a:p></p:txBody></p:sp>"
    )
    rotated = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"31\" name=\"Rotated\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>"
        '<p:spPr><a:xfrm rot="5400000"><a:off x="7200000" y="1900000"/><a:ext cx="600000" cy="1400000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFC000"/></a:solidFill></p:spPr>'
        '<p:txBody><a:bodyPr vert="vert"/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Rotated</a:t></a:r></a:p></p:txBody></p:sp>'
    )
    flipped = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"32\" name=\"Flipped\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>"
        '<p:spPr><a:xfrm flipH="1"><a:off x="8000000" y="1900000"/><a:ext cx="800000" cy="500000"/></a:xfrm>'
        '<a:prstGeom prst="rightArrow"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="9BBB59"/></a:solidFill></p:spPr></p:sp>'
    )
    custom = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"33\" name=\"Custom\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>"
        '<p:spPr><a:xfrm><a:off x="2600000" y="1900000"/><a:ext cx="800000" cy="500000"/></a:xfrm>'
        '<a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l="0" t="0" r="r" b="b"/>'
        '<a:pathLst><a:path w="1000" h="1000"><a:moveTo><a:pt x="0" y="1000"/></a:moveTo>'
        '<a:lnTo><a:pt x="500" y="0"/></a:lnTo><a:lnTo><a:pt x="1000" y="1000"/></a:lnTo><a:close/></a:path></a:pathLst></a:custGeom>'
        '<a:solidFill><a:srgbClr val="FFC000"/></a:solidFill></p:spPr></p:sp>'
    )
    inherited = (
        "<p:sp><p:nvSpPr><p:cNvPr id=\"11\" name=\"Inherited title\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr>"
        "<p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>"
        '<a:p><a:r><a:t>Inherited title</a:t></a:r></a:p></p:txBody></p:sp>'
    )
    return (
        f'<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        + inherited
        + rect(10, 400000, 300000, 4000000, 600000, fill='<a:solidFill><a:srgbClr val="1F497D"/></a:solidFill>', text="标题")
        + group
        + style_shape
        + rotated
        + flipped
        + custom
        + connector(34, 400000, 2400000, 1200000, 200000)
        + pic(40, 400000, 1100000, 2400000, 1400000, "rIdImg", (10000, 20000, 0, 30000))
        + table(50, 400000, 2800000, 5000000, 1300000)
        + chart_frame(60, 5700000, 2800000, 3000000, 2200000, "rIdChart1")
        + chart_frame(61, 1000000, 4400000, 3000000, 1800000, "rIdChart2")
        + "</p:spTree></p:cSld>"
        '<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>'
    )

def slide2():
    return (
        f'<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld>'
        '<p:bg><p:bgPr><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="EEF3FF"/></a:gs>'
        '<a:gs pos="100000"><a:srgbClr val="FFF4E8"/></a:gs></a:gsLst><a:lin ang="2700000"/></a:gradFill></p:bgPr></p:bg>'
        '<p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        + rect(10, 400000, 300000, 5000000, 500000, text="More charts")
        + chart_frame(60, 500000, 1000000, 3000000, 2200000, "rIdChart3")
        + chart_frame(61, 3800000, 1000000, 3000000, 2200000, "rIdChart4")
        + chart_frame(62, 500000, 3400000, 3000000, 2200000, "rIdChart5")
        + chart_frame(63, 3800000, 3400000, 3000000, 2200000, "rIdChart6")
        + diagram_frame(70, 500000, 1500000, 2000000, 900000)
        + ole_frame(71, 6800000, 5200000, 1000000, 500000, "rIdOle")
        + "</p:spTree></p:cSld>"
        '<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>'
    )

def theme():
    return (
        f'<a:theme xmlns:a="{A}" name="Fixture"><a:themeElements>'
        '<a:clrScheme name="Fixture">'
        '<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>'
        '<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>'
        '<a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>'
        '<a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2>'
        '<a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4>'
        '<a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6>'
        '<a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink>'
        "</a:clrScheme>"
        '<a:fontScheme name="Fixture"><a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>'
        '<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface="宋体"/><a:cs typeface=""/></a:minorFont></a:fontScheme>'
        '<a:fmtScheme name="Fixture">'
        '<a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
        '<a:gradFill><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:tint val="50000"/></a:schemeClr></a:gs>'
        '<a:gs pos="100000"><a:schemeClr val="phClr"><a:shade val="80000"/></a:schemeClr></a:gs></a:gsLst>'
        '<a:lin ang="5400000"/></a:gradFill>'
        '<a:gradFill><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"/></a:gs><a:gs pos="100000"><a:schemeClr val="phClr"><a:tint val="40000"/></a:schemeClr></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill>'
        "</a:fillStyleLst>"
        '<a:lnStyleLst><a:ln w="9525"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>'
        '<a:ln w="25400"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>'
        '<a:ln w="38100"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln></a:lnStyleLst>'
        '<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>'
        '<a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>'
        "</a:fmtScheme></a:themeElements></a:theme>"
    )

def master():
    return (
        f'<p:sldMaster xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld>'
        '<p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg>'
        '<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>'
        '<p:spPr><a:xfrm><a:off x="400000" y="200000"/><a:ext cx="8000000" cy="800000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Master title</a:t></a:r></a:p></p:txBody></p:sp>'
        + pic(3, 7600000, 200000, 900000, 600000, "rIdLogo")
        + "</p:spTree></p:cSld>"
        '<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>'
        '<p:txStyles><p:titleStyle><a:lvl1pPr algn="ctr"><a:defRPr sz="3200" b="1"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="Calibri Light"/></a:defRPr></a:lvl1pPr></p:titleStyle>'
        '<p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:defRPr sz="1800"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="Calibri"/><a:ea typeface="宋体"/></a:defRPr><a:buChar char="•"/></a:lvl1pPr></p:bodyStyle>'
        '<p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle></p:txStyles>'
        "</p:sldMaster>"
    )

def layout():
    return (
        f'<p:sldLayout xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}" type="titleOnly"><p:cSld name="Title Only">'
        '<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        '<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>'
        '<p:spPr><a:xfrm><a:off x="400000" y="200000"/><a:ext cx="8000000" cy="800000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>'
        '<p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle><a:lvl1pPr algn="l"><a:defRPr sz="3000" b="1" i="1"><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:defRPr></a:lvl1pPr></a:lstStyle>'
        '<a:p><a:r><a:rPr lang="en-US"/><a:t>Layout title</a:t></a:r></a:p></p:txBody></p:sp>'
        + rect(3, 400000, 4700000, 8400000, 300000, fill='<a:solidFill><a:srgbClr val="C0504D"/></a:solidFill>')
        + "</p:spTree></p:cSld></p:sldLayout>"
    )

def rels(items):
    return (
        f'<Relationships xmlns="{REL}">'
        + "".join(
            f'<Relationship Id="{i}" Type="{R}/{t}" Target="{target}"/>'
            for i, t, target in items
        )
        + "</Relationships>"
    )

def main():
    charts = {
        "ppt/charts/chart1.xml": chart("barChart", ["A", "B", "C"], [
            ser("One", ["A", "B", "C"], [3, 5, 2], "4472C4"),
            ser("Two", ["A", "B", "C"], [2, 4, 1], "ED7D31"),
        ], "Column", labels=True),
        "ppt/charts/chart2.xml": chart("pieChart", ["A", "B", "C"], [
            ser("Share", ["A", "B", "C"], [40, 35, 25], "4472C4"),
        ], "Pie"),
        "ppt/charts/chart3.xml": chart("lineChart", ["A", "B", "C", "D"], [
            ser("Line", ["A", "B", "C", "D"], [1, 4, 2, 6], "5B9BD5"),
        ], "Line", labels=True),
        "ppt/charts/chart4.xml": chart("areaChart", ["A", "B", "C", "D"], [
            ser("Area", ["A", "B", "C", "D"], [4, 2, 5, 3], "70AD47"),
        ], "Area", grouping="stacked"),
        "ppt/charts/chart5.xml": chart("barChart", ["A", "B", "C"], [
            ser("Bar", ["A", "B", "C"], [5, 3, 4], "8064A2"),
        ], "Bar", grouping="percentStacked", bar_dir="bar"),
        "ppt/charts/chart6.xml": chart("doughnutChart", ["A", "B", "C", "D"], [
            ser("Ring", ["A", "B", "C", "D"], [3, 4, 2, 1], "F79646"),
        ], "Doughnut"),
        "ppt/diagrams/data1.xml": (
            f'<dgm:dataModel xmlns:dgm="{DGM}" xmlns:a="{A}"><dgm:ptLst><dgm:pt modelId="1"/></dgm:ptLst></dgm:dataModel>'
        ),
        "ppt/diagrams/drawing1.xml": (
            f'<dsp:drawing xmlns:dsp="{DGM}" xmlns:a="{A}"><dsp:spTree>'
            '<dsp:sp><dsp:nvSpPr><dsp:cNvPr id="1" name="Step"/><dsp:cNvSpPr/><dsp:nvPr/></dsp:nvSpPr>'
            '<dsp:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="500000"/></a:xfrm>'
            '<a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom>'
            '<a:solidFill><a:srgbClr val="4472C4"/></a:solidFill></dsp:spPr>'
            '<dsp:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>SmartArt step</a:t></a:r></a:p></dsp:txBody></dsp:sp>'
            "</dsp:spTree></dsp:drawing>"
        ),
    }
    with zipfile.ZipFile(OUT, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", (
            f'<Types xmlns="{CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
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
                f'<Override PartName="/{name}" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>'
                for name in charts
            )
            + "</Types>"
        ))
        z.writestr("_rels/.rels", rels([("rId1", "officeDocument", "ppt/presentation.xml")]))
        z.writestr("ppt/presentation.xml", (
            f'<p:presentation xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}">'
            '<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
            '<p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst>'
            '<p:sldSz cx="9144000" cy="5143500"/><p:notesSz cx="6858000" cy="9144000"/>'
            "</p:presentation>"
        ))
        z.writestr("ppt/_rels/presentation.xml.rels", rels([
            ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
            ("rId2", "slide", "slides/slide1.xml"),
            ("rId3", "slide", "slides/slide2.xml"),
            ("rId4", "theme", "theme/theme1.xml"),
            ("rId5", "tableStyles", "tableStyles.xml"),
        ]))
        z.writestr("ppt/theme/theme1.xml", theme())
        z.writestr("ppt/tableStyles.xml", f'<a:tblStyleLst xmlns:a="{A}" def="{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}"/>')
        z.writestr("ppt/slideMasters/slideMaster1.xml", master())
        z.writestr("ppt/slideMasters/_rels/slideMaster1.xml.rels", rels([
            ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rId2", "theme", "../theme/theme1.xml"),
            ("rIdLogo", "image", "../media/logo.png"),
        ]))
        z.writestr("ppt/slideLayouts/slideLayout1.xml", layout())
        z.writestr("ppt/slideLayouts/_rels/slideLayout1.xml.rels", rels([
            ("rId1", "slideMaster", "../slideMasters/slideMaster1.xml"),
        ]))
        z.writestr("ppt/slides/slide1.xml", slide1())
        z.writestr("ppt/slides/_rels/slide1.xml.rels", rels([
            ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rIdImg", "image", "../media/logo.png"),
            ("rIdChart1", "chart", "../charts/chart1.xml"),
            ("rIdChart2", "chart", "../charts/chart2.xml"),
        ]))
        z.writestr("ppt/slides/slide2.xml", slide2())
        z.writestr("ppt/slides/_rels/slide2.xml.rels", rels([
            ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rIdChart3", "chart", "../charts/chart3.xml"),
            ("rIdChart4", "chart", "../charts/chart4.xml"),
            ("rIdChart5", "chart", "../charts/chart5.xml"),
            ("rIdChart6", "chart", "../charts/chart6.xml"),
            ("rIdDgm", "diagramData", "../diagrams/data1.xml"),
            ("rIdOle", "image", "../media/logo.png"),
        ]))
        z.writestr("ppt/diagrams/_rels/data1.xml.rels", rels([
            ("rIdDraw", "diagramDrawing", "drawing1.xml"),
        ]))
        for name, xml in charts.items():
            z.writestr(name, xml)
        z.writestr("ppt/media/logo.png", PNG)
    print(OUT)

if __name__ == "__main__":
    main()
