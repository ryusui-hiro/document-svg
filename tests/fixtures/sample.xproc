<?xml version="1.0" encoding="UTF-8"?>
<p:declare-step xmlns:p="http://www.w3.org/ns/xproc" version="3.0" name="private-pipeline"><p:input port="source"/><p:output port="result"/><p:option name="secret" select="'private'"/><p:load href="private.xml"/><p:xslt><p:with-input port="stylesheet" href="private.xsl"/></p:xslt><p:store href="https://example.invalid/out.xml"/><p:choose><p:when test="private"><p:identity/></p:when></p:choose></p:declare-step>

