#!/usr/bin/env python3
"""Build the sheet fixtures with the Python standard library only.

These are synthetic OOXML packages written by this script. They follow the
parts Excel and WPS emit, but they were not produced or validated by either
application. See PROVENANCE.md.
"""

import base64
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk"
    "+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
)

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
XDR = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
PKG = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"


def theme(accent="4472C4", latin="Calibri", ea="等线"):
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="{A}" name="Office">
 <a:themeElements>
  <a:clrScheme name="Office">
   <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
   <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
   <a:dk2><a:srgbClr val="44546A"/></a:dk2>
   <a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
   <a:accent1><a:srgbClr val="{accent}"/></a:accent1>
   <a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
   <a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>
   <a:accent4><a:srgbClr val="FFC000"/></a:accent4>
   <a:accent5><a:srgbClr val="5B9BD5"/></a:accent5>
   <a:accent6><a:srgbClr val="70AD47"/></a:accent6>
   <a:hlink><a:srgbClr val="0563C1"/></a:hlink>
   <a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
  </a:clrScheme>
  <a:fontScheme name="Office">
   <a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface="{ea}"/><a:cs typeface=""/></a:majorFont>
   <a:minorFont><a:latin typeface="{latin}"/><a:ea typeface="{ea}"/><a:cs typeface=""/></a:minorFont>
  </a:fontScheme>
  <a:fmtScheme name="Office"><a:fillStyleLst/><a:lnStyleLst/><a:effectStyleLst/><a:bgFillStyleLst/></a:fmtScheme>
 </a:themeElements>
</a:theme>"""


def styles(theme_fill="theme=\"4\" tint=\"0.3999755851924192\"", font="Calibri"):
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS}">
 <numFmts count="5">
  <numFmt numFmtId="164" formatCode="yyyy&quot;年&quot;m&quot;月&quot;d&quot;日&quot;"/>
  <numFmt numFmtId="165" formatCode="[Red]0.00"/>
  <numFmt numFmtId="166" formatCode="0.00;[Red]-0.00"/>
  <numFmt numFmtId="167" formatCode="&quot;¥&quot;#,##0.00"/>
  <numFmt numFmtId="168" formatCode="# ?/?"/>
 </numFmts>
 <fonts count="4">
  <font><sz val="11"/><color theme="1"/><name val="{font}"/><family val="2"/><scheme val="minor"/></font>
  <font><b/><sz val="12"/><color rgb="FFFF0000"/><name val="宋体"/><charset val="134"/></font>
  <font><i/><sz val="11"/><color rgb="FF0000FF"/><name val="等线"/><charset val="134"/></font>
  <font><b/><sz val="11"/><color rgb="FFFFFFFF"/><name val="{font}"/></font>
 </fonts>
 <fills count="4">
  <fill><patternFill patternType="none"/></fill>
  <fill><patternFill patternType="gray125"/></fill>
  <fill><patternFill patternType="solid"><fgColor {theme_fill}/><bgColor indexed="64"/></patternFill></fill>
  <fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/><bgColor indexed="64"/></patternFill></fill>
 </fills>
 <borders count="3">
  <border><left/><right/><top/><bottom/><diagonal/></border>
  <border><left style="thin"><color theme="1"/></left><right style="thin"><color theme="1"/></right><top style="thin"><color theme="1"/></top><bottom style="thin"><color theme="1"/></bottom><diagonal/></border>
  <border><left/><right/><top/><bottom style="double"><color rgb="FF000000"/></bottom><diagonal/></border>
 </borders>
 <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
 <cellXfs count="16">
  <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
  <xf numFmtId="0" fontId="1" fillId="2" borderId="1" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center" vertical="center" wrapText="1"/></xf>
  <xf numFmtId="10" fontId="2" fillId="0" borderId="0" xfId="0" applyNumberFormat="1" applyFont="1"/>
  <xf numFmtId="31" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="166" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="167" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="168" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="4" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="11" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="0" fontId="3" fillId="3" borderId="2" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf>
  <xf numFmtId="18" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="34" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="6" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="4" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
 </cellXfs>
 <cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
 <dxfs count="6">
  <dxf><fill><patternFill><fgColor rgb="FFC6EFCE"/></patternFill></fill></dxf>
  <dxf><font><b/><color rgb="FF9C0006"/></font></dxf>
  <dxf><fill><patternFill><fgColor rgb="FFFFEB9C"/></patternFill></fill></dxf>
  <dxf><fill><patternFill><fgColor rgb="FFF4B084"/></patternFill></fill></dxf>
  <dxf><font><color rgb="FF0000FF"/></font></dxf>
  <dxf><fill><patternFill><fgColor rgb="FFBDD7EE"/></patternFill></fill></dxf>
 </dxfs>
 <tableStyles count="0" defaultTableStyle="TableStyleMedium2" defaultPivotStyle="PivotStyleLight16"/>
</styleSheet>"""


def workbook():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS}" xmlns:r="{R}">
 <workbookPr/>
 <sheets><sheet name="预算" sheetId="1" r:id="rId1"/></sheets>
 <calcPr calcId="191029"/>
</workbook>"""


def workbook_rels():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}">
 <Relationship Id="rId1" Type="{R}/worksheet" Target="worksheets/sheet1.xml"/>
 <Relationship Id="rId2" Type="{R}/styles" Target="styles.xml"/>
 <Relationship Id="rId3" Type="{R}/sharedStrings" Target="sharedStrings.xml"/>
 <Relationship Id="rId4" Type="{R}/theme" Target="theme/theme1.xml"/>
</Relationships>"""


SHARED = ["预算", "说明", "备注"]


def shared_strings(items):
    body = "".join(
        f'<si><t xml:space="preserve">{s}</t></si>' for s in items
    )
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS}" count="{len(items)}" uniqueCount="{len(items)}">{body}</sst>"""


def sheet(sheet_style=1, wps=False):
    tab = 'theme="4" tint="0.3999755851924192"'
    if wps:
        tab = 'indexed="12"'
    pane = 'xSplit="1" ySplit="1" topLeftCell="B2" activePane="bottomRight" state="frozen"'
    rows = [
        '<row r="1" ht="24" customHeight="1">'
        '<c r="A1" s="1" t="s"><v>0</v></c>'
        '<c r="B1" s="1"/>'
        '<c r="C1" s="1"/>'
        '<c r="D1" s="11" t="s"><v>2</v></c>'
        '</row>',
        '<row r="2">'
        '<c r="A2" s="2"><v>0.25</v></c>'
        '<c r="B2" s="9"><v>1234.5</v></c>'
        '<c r="C2" s="10"><v>0.000125</v></c>'
        '<c r="D2" s="14"><v>1234.5</v></c>'
        '</row>',
        '<row r="3" hidden="1">'
        '<c r="A3" s="8"><v>1.25</v></c>'
        '<c r="B3" s="3"><v>45356</v></c>'
        '<c r="C3" s="12"><v>45356.625</v></c>'
        '<c r="D3" s="15"><f>SUM(B2:B3)</f><v>1234.5</v></c>'
        '</row>',
        '<row r="4">'
        '<c r="A4" s="4"><v>45356</v></c>'
        '<c r="B4" s="6"><v>-12.5</v></c>'
        '<c r="C4" s="7"><v>1234.5</v></c>'
        '<c r="D4" s="14"><v>12.5</v></c>'
        '</row>',
        f'<row r="5" ht="30" customHeight="1" s="{sheet_style}" customFormat="1"><c r="A5"><v>100</v></c></row>',
        '<row r="6"><c r="A6" s="0" t="s"><v>1</v></c><c r="B6"><v>10</v></c></row>',
        '<row r="7"><c r="A7"><v>1</v></c><c r="B7"><v>2</v></c><c r="C7"><v>3</v></c><c r="D7"><v>5</v></c></row>',
        '<row r="8"><c r="A8"><v>5</v></c><c r="B8"><v>10</v></c><c r="C8"><v>9</v></c><c r="D8"><v>10</v></c><c r="E8" t="s"><v>1</v></c></row>',
        '<row r="9" hidden="1"><c r="A9"><v>5</v></c><c r="C9"><v>8</v></c></row>',
    ]
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS}" xmlns:r="{R}">
 <sheetPr><tabColor {tab}/></sheetPr>
 <dimension ref="A1:F14"/>
 <sheetViews><sheetView showGridLines="0" zoomScale="125" workbookViewId="0"><pane {pane}/></sheetView></sheetViews>
 <sheetFormatPr defaultColWidth="8.43" defaultRowHeight="15"/>
 <cols>
  <col min="1" max="1" width="20" customWidth="1"/>
  <col min="2" max="2" width="12" customWidth="1"/>
  <col min="3" max="3" width="8.43" hidden="1" customWidth="1"/>
  <col min="4" max="4" width="18" customWidth="1"/>
 </cols>
 <sheetData>{"".join(rows)}</sheetData>
 <mergeCells count="1"><mergeCell ref="A1:C1"/></mergeCells>
 <conditionalFormatting sqref="D8"><cfRule type="cellIs" operator="greaterThan" priority="1" dxfId="0"><formula>0</formula></cfRule></conditionalFormatting>
 <conditionalFormatting sqref="A6"><cfRule type="containsText" operator="containsText" text="明" priority="2" dxfId="1"><formula>NOT(ISERROR(SEARCH("明",A6)))</formula></cfRule></conditionalFormatting>
 <conditionalFormatting sqref="B6:B7"><cfRule type="top10" rank="1" priority="3" dxfId="2"/></conditionalFormatting>
 <conditionalFormatting sqref="B8:C8"><cfRule type="aboveAverage" priority="4" dxfId="3"/></conditionalFormatting>
 <conditionalFormatting sqref="A8:A9"><cfRule type="duplicateValues" priority="5" dxfId="4"/></conditionalFormatting>
 <conditionalFormatting sqref="C8"><cfRule type="uniqueValues" priority="6" dxfId="5"/></conditionalFormatting>
 <conditionalFormatting sqref="A7:C7"><cfRule type="colorScale" priority="7"><colorScale><cfvo type="min"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FF63BE7B"/></colorScale></cfRule></conditionalFormatting>
 <conditionalFormatting sqref="D7:D8"><cfRule type="dataBar" priority="8"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar></cfRule></conditionalFormatting>
 <conditionalFormatting sqref="E8"><cfRule type="expression" priority="9"><formula>TRUE</formula></cfRule></conditionalFormatting>
 <drawing r:id="rId1"/>
</worksheet>"""


def drawing():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xdr:wsDr xmlns:xdr="{XDR}" xmlns:a="{A}" xmlns:r="{R}" xmlns:c="{C}">
 <xdr:twoCellAnchor>
  <xdr:from><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>9</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
  <xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>11</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
  <xdr:pic>
   <xdr:nvPicPr><xdr:cNvPr id="2" name="图片 1"/><xdr:cNvPicPr/></xdr:nvPicPr>
   <xdr:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill>
   <xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="1500000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr>
  </xdr:pic>
  <xdr:clientData/>
 </xdr:twoCellAnchor>
 <xdr:oneCellAnchor>
  <xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>12</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
  <xdr:ext cx="4000000" cy="2500000"/>
  <xdr:graphicFrame macro="">
   <xdr:nvGraphicFramePr><xdr:cNvPr id="3" name="图表 1"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>
   <xdr:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="2500000"/></xdr:xfrm>
   <a:graphic><a:graphicData uri="{C}"><c:chart r:id="rId2"/></a:graphicData></a:graphic>
  </xdr:graphicFrame>
  <xdr:clientData/>
 </xdr:oneCellAnchor>
 <xdr:twoCellAnchor>
  <xdr:from><xdr:col>7</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
  <xdr:to><xdr:col>9</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>3</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
  <xdr:sp macro="" textlink="">
   <xdr:nvSpPr><xdr:cNvPr id="4" name="文本框 1"/><xdr:cNvSpPr txBox="1"/></xdr:nvSpPr>
   <xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="1000000"/></a:xfrm><a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFF2F2F2"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="FF4472C4"/></a:solidFill></a:ln></xdr:spPr>
   <xdr:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN"/><a:t>备注</a:t></a:r></a:p></xdr:txBody>
  </xdr:sp>
  <xdr:clientData/>
 </xdr:twoCellAnchor>
</xdr:wsDr>"""


def drawing_rels():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}">
 <Relationship Id="rId1" Type="{R}/image" Target="../media/image1.png"/>
 <Relationship Id="rId2" Type="{R}/chart" Target="../charts/chart1.xml"/>
</Relationships>"""


def chart(theme_color="4472C4"):
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="{C}" xmlns:a="{A}" xmlns:r="{R}">
 <c:chart>
  <c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>季度</a:t></a:r></a:p></c:rich></c:tx></c:title>
  <c:plotArea><c:layout/>
   <c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>
    <c:ser><c:idx val="0"/><c:order val="0"/>
     <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>收入</c:v></c:pt></c:strCache></c:strRef></c:tx>
     <c:spPr><a:solidFill><a:srgbClr val="{theme_color}"/></a:solidFill></c:spPr>
     <c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt><c:pt idx="2"><c:v>Q3</c:v></c:pt></c:strCache></c:strRef></c:cat>
     <c:val><c:numRef><c:numCache><c:formatCode>General</c:formatCode><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt><c:pt idx="2"><c:v>30</c:v></c:pt></c:numCache></c:numRef></c:val>
    </c:ser>
    <c:axId val="1"/><c:axId val="2"/>
   </c:barChart>
  </c:plotArea>
  <c:legend><c:legendPos val="r"/></c:legend>
 </c:chart>
</c:chartSpace>"""


def content_types():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="{CT}">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="xml" ContentType="application/xml"/>
 <Default Extension="png" ContentType="image/png"/>
 <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
 <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
 <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
 <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
 <Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>
 <Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>
 <Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>
</Types>"""


def package_rels():
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}">
 <Relationship Id="rId1" Type="{R}/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""


def write(path, wps=False):
    accent = "2F5597" if wps else "4472C4"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", content_types())
        z.writestr("_rels/.rels", package_rels())
        z.writestr("xl/workbook.xml", workbook())
        z.writestr("xl/_rels/workbook.xml.rels", workbook_rels())
        z.writestr("xl/theme/theme1.xml", theme(accent=accent, latin="等线" if wps else "Calibri", ea="宋体" if wps else "等线"))
        z.writestr("xl/styles.xml", styles(theme_fill='indexed="10"' if wps else 'theme="4" tint="0.3999755851924192"'))
        z.writestr("xl/sharedStrings.xml", shared_strings(SHARED))
        z.writestr("xl/worksheets/sheet1.xml", sheet(wps=wps))
        z.writestr("xl/worksheets/_rels/sheet1.xml.rels", f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG}"><Relationship Id="rId1" Type="{R}/drawing" Target="../drawings/drawing1.xml"/></Relationships>""")
        z.writestr("xl/drawings/drawing1.xml", drawing())
        z.writestr("xl/drawings/_rels/drawing1.xml.rels", drawing_rels())
        z.writestr("xl/charts/chart1.xml", chart(accent))
        z.writestr("xl/media/image1.png", PNG)


if __name__ == "__main__":
    write(os.path.join(HERE, "excel-style.xlsx"))
    write(os.path.join(HERE, "wps-style.xlsx"), wps=True)
